use crate::characters::{CharacterKind, CharacterMoveCatalog, CharacterMoveSlot};
use crate::components::{AttackKind, FighterAction};
use crate::constants::*;
use crate::equipment::{
    EquipmentKind, LoadoutContext, LoadoutTag, loadout_has_tag, loadout_technique_modifier,
};
use crate::reactions::ReactionFamilyId;
use crate::styles::FighterStyleKind;

pub const MS_PER_SECOND: f32 = 1000.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
pub enum TechniqueId {
    CatLight1,
    CatLight2,
    CatComboFinisher,
    CatDashComboFinisher,
    CatHeavy,
    CatHeavy2,
    CatDashAttack,
    CatJumpAttack,
    CatJumpHeavy,
    CatUltimateStartup,
    CatUltimateRush,
    PigLight1,
    PigLight2,
    PigComboFinisher,
    PigHeavy,
    PigHeavy2,
    PigDashAttack,
    PigJumpAttack,
    PigJumpHeavy,
    PigUltimateStartup,
    PigUltimateRush,
    DogLight1,
    DogLight2,
    DogComboFinisher,
    DogHeavy,
    DogHeavy2,
    DogDashAttack,
    DogJumpAttack,
    DogJumpHeavy,
    DogUltimateStartup,
    DogUltimateRush,
    FoxLight1,
    FoxLight2,
    FoxComboFinisher,
    FoxHeavy,
    FoxHeavy2,
    FoxDashAttack,
    FoxJumpAttack,
    FoxJumpHeavy,
    FoxUltimateStartup,
    FoxUltimateRush,
    PandaLight1,
    PandaLight2,
    PandaComboFinisher,
    PandaHeavy,
    PandaHeavy2,
    PandaDashAttack,
    PandaJumpAttack,
    PandaJumpHeavy,
    PandaUltimateStartup,
    PandaUltimateRush,
    BeeLight1,
    BeeLight2,
    BeeComboFinisher,
    BeeHeavy,
    BeeHeavy2,
    BeeDashAttack,
    BeeJumpAttack,
    BeeJumpHeavy,
    BeeUltimateStartup,
    BeeLegacyUltimateStartup,
    BeeLegacyUltimateRush,
    PenguinLight1,
    PenguinLight2,
    PenguinComboFinisher,
    PenguinHeavy,
    PenguinHeavy2,
    PenguinDashAttack,
    PenguinDashHeavy,
    PenguinJumpAttack,
    PenguinJumpHeavy,
    PenguinUltimateStartup,
    PenguinUltimateRush,
    ChickLight1,
    ChickLight2,
    ChickComboFinisher,
    ChickHeavy,
    ChickHeavy2,
    ChickDashAttack,
    ChickDashHeavy,
    ChickJumpAttack,
    ChickJumpHeavy,
    ChickUltimateStartup,
    Grab,
    Throw,
    GuardCounter,
    SpecialCast,
    ItemPickup,
    ItemSwing,
    ItemThrow,
    ItemDrop,
    GuardStep,
    QuickStand,
    RecoveryRoll,
    LandingRecovery,
}

impl TechniqueId {
    pub fn owner(self) -> Option<CharacterKind> {
        match self {
            Self::CatLight1
            | Self::CatLight2
            | Self::CatComboFinisher
            | Self::CatDashComboFinisher
            | Self::CatHeavy
            | Self::CatHeavy2
            | Self::CatDashAttack
            | Self::CatJumpAttack
            | Self::CatJumpHeavy
            | Self::CatUltimateStartup
            | Self::CatUltimateRush => Some(CharacterKind::Cat),
            Self::PigLight1
            | Self::PigLight2
            | Self::PigComboFinisher
            | Self::PigHeavy
            | Self::PigHeavy2
            | Self::PigDashAttack
            | Self::PigJumpAttack
            | Self::PigJumpHeavy
            | Self::PigUltimateStartup
            | Self::PigUltimateRush => Some(CharacterKind::Pig),
            Self::DogLight1
            | Self::DogLight2
            | Self::DogComboFinisher
            | Self::DogHeavy
            | Self::DogHeavy2
            | Self::DogDashAttack
            | Self::DogJumpAttack
            | Self::DogJumpHeavy
            | Self::DogUltimateStartup
            | Self::DogUltimateRush => Some(CharacterKind::Dog),
            Self::FoxLight1
            | Self::FoxLight2
            | Self::FoxComboFinisher
            | Self::FoxHeavy
            | Self::FoxHeavy2
            | Self::FoxDashAttack
            | Self::FoxJumpAttack
            | Self::FoxJumpHeavy
            | Self::FoxUltimateStartup
            | Self::FoxUltimateRush => Some(CharacterKind::Fox),
            Self::PandaLight1
            | Self::PandaLight2
            | Self::PandaComboFinisher
            | Self::PandaHeavy
            | Self::PandaHeavy2
            | Self::PandaDashAttack
            | Self::PandaJumpAttack
            | Self::PandaJumpHeavy
            | Self::PandaUltimateStartup
            | Self::PandaUltimateRush => Some(CharacterKind::Panda),
            Self::BeeLight1
            | Self::BeeLight2
            | Self::BeeComboFinisher
            | Self::BeeHeavy
            | Self::BeeHeavy2
            | Self::BeeDashAttack
            | Self::BeeJumpAttack
            | Self::BeeJumpHeavy
            | Self::BeeUltimateStartup
            | Self::BeeLegacyUltimateStartup
            | Self::BeeLegacyUltimateRush => Some(CharacterKind::Bee),
            Self::PenguinLight1
            | Self::PenguinLight2
            | Self::PenguinComboFinisher
            | Self::PenguinHeavy
            | Self::PenguinHeavy2
            | Self::PenguinDashAttack
            | Self::PenguinDashHeavy
            | Self::PenguinJumpAttack
            | Self::PenguinJumpHeavy
            | Self::PenguinUltimateStartup
            | Self::PenguinUltimateRush => Some(CharacterKind::Penguin),
            Self::ChickLight1
            | Self::ChickLight2
            | Self::ChickComboFinisher
            | Self::ChickHeavy
            | Self::ChickHeavy2
            | Self::ChickDashAttack
            | Self::ChickDashHeavy
            | Self::ChickJumpAttack
            | Self::ChickJumpHeavy
            | Self::ChickUltimateStartup => Some(CharacterKind::Chick),
            Self::Grab
            | Self::Throw
            | Self::GuardCounter
            | Self::SpecialCast
            | Self::ItemPickup
            | Self::ItemSwing
            | Self::ItemThrow
            | Self::ItemDrop
            | Self::GuardStep
            | Self::QuickStand
            | Self::RecoveryRoll
            | Self::LandingRecovery => None,
        }
    }

    pub fn allowed_for_character(self, character: CharacterKind) -> bool {
        self.owner().is_none_or(|owner| owner == character)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::CatLight1 => "cat_A_s",
            Self::CatLight2 => "cat_A_ss",
            Self::CatComboFinisher => "cat_combo_finisher",
            Self::CatDashComboFinisher => "cat_dash_combo_finisher",
            Self::CatHeavy => "cat_X_step",
            Self::CatHeavy2 => "cat_kiriage",
            Self::CatDashAttack => "cat_dash_attack",
            Self::CatJumpAttack => "cat_jump_attack",
            Self::CatJumpHeavy => "cat_jump_x_fish",
            Self::CatUltimateStartup => "cat_ultimate_startup",
            Self::CatUltimateRush => "cat_ultimate_rush",
            Self::PigLight1 => "pig_cat_light_1",
            Self::PigLight2 => "pig_cat_light_2",
            Self::PigComboFinisher => "pig_ham_slam",
            Self::PigHeavy => "pig_half_circle_swing",
            Self::PigHeavy2 => "pig_ham_launcher",
            Self::PigDashAttack => "pig_dash_attack",
            Self::PigJumpAttack => "pig_cat_jump_attack",
            Self::PigJumpHeavy => "pig_air_meat_slam",
            Self::PigUltimateStartup => "pig_unblockable_grab_startup",
            Self::PigUltimateRush => "pig_unblockable_grab_rush",
            Self::DogLight1 => "dog_bite_1",
            Self::DogLight2 => "dog_bite_2",
            Self::DogComboFinisher => "dog_body_pounce",
            Self::DogHeavy => "dog_shoulder_step",
            Self::DogHeavy2 => "dog_launch_bite",
            Self::DogDashAttack => "dog_dash_attack",
            Self::DogJumpAttack => "dog_air_pounce",
            Self::DogJumpHeavy => "dog_air_fish",
            Self::DogUltimateStartup => "dog_ultimate_startup",
            Self::DogUltimateRush => "dog_ultimate_rush",
            Self::FoxLight1 => "fox_swipe_1",
            Self::FoxLight2 => "fox_swipe_2",
            Self::FoxComboFinisher => "fox_tail_sweep",
            Self::FoxHeavy => "fox_skitter_step",
            Self::FoxHeavy2 => "fox_flip_launch",
            Self::FoxDashAttack => "fox_dash_attack",
            Self::FoxJumpAttack => "fox_air_swipe",
            Self::FoxJumpHeavy => "fox_air_fish",
            Self::FoxUltimateStartup => "fox_ultimate_startup",
            Self::FoxUltimateRush => "fox_ultimate_rush",
            Self::PandaLight1 => "panda_palm_1",
            Self::PandaLight2 => "panda_palm_2",
            Self::PandaComboFinisher => "panda_body_drop",
            Self::PandaHeavy => "panda_weight_shift",
            Self::PandaHeavy2 => "panda_rising_scoop",
            Self::PandaDashAttack => "panda_dash_attack",
            Self::PandaJumpAttack => "panda_air_drop",
            Self::PandaJumpHeavy => "panda_air_fish",
            Self::PandaUltimateStartup => "panda_ultimate_startup",
            Self::PandaUltimateRush => "panda_ultimate_rush",
            Self::BeeLight1 => "bee_worker_swarm",
            Self::BeeLight2 => "bee_cross_sting",
            Self::BeeComboFinisher => "bee_spiral_sting",
            Self::BeeHeavy => "bee_piercing_step",
            Self::BeeHeavy2 => "bee_homing_sting",
            Self::BeeDashAttack => "bee_dash_attack",
            Self::BeeJumpAttack => "bee_air_dash",
            Self::BeeJumpHeavy => "bee_hive_dive",
            Self::BeeUltimateStartup => "bee_ultimate_startup",
            Self::BeeLegacyUltimateStartup => "bee_legacy_ultimate_startup",
            Self::BeeLegacyUltimateRush => "bee_legacy_ultimate_rush",
            Self::PenguinLight1 => "penguin_snowflake_shot",
            Self::PenguinLight2 => "penguin_snowflake_followup",
            Self::PenguinComboFinisher => "penguin_belly_slide",
            Self::PenguinHeavy => "penguin_snowman_drop",
            Self::PenguinHeavy2 => "penguin_sled_scoop",
            Self::PenguinDashAttack => "penguin_dash_snowflake_shot",
            Self::PenguinDashHeavy => "penguin_snow_slope_slide",
            Self::PenguinJumpAttack => "penguin_air_snowflake_shot",
            Self::PenguinJumpHeavy => "penguin_snowflake_warp",
            Self::PenguinUltimateStartup => "penguin_ultimate_startup",
            Self::PenguinUltimateRush => "penguin_ultimate_rush",
            Self::ChickLight1 => "chick_orbit_egg_launch",
            Self::ChickLight2 => "chick_sunny_flip",
            Self::ChickComboFinisher => "chick_shell_scramble",
            Self::ChickHeavy => "chick_orbit_egg",
            Self::ChickHeavy2 => "chick_eggplant_impostor",
            Self::ChickDashAttack => "chick_dash_backstep_c",
            Self::ChickDashHeavy => "chick_dash_backstep_x",
            Self::ChickJumpAttack => "chick_updraft_glide",
            Self::ChickJumpHeavy => "chick_fresh_egg_ride",
            Self::ChickUltimateStartup => "chick_egg_burst",
            Self::Grab => "grab",
            Self::Throw => "throw",
            Self::GuardCounter => "guard_counter",
            Self::SpecialCast => "special_cast",
            Self::ItemPickup => "item_pickup",
            Self::ItemSwing => "item_swing",
            Self::ItemThrow => "item_throw",
            Self::ItemDrop => "item_drop",
            Self::GuardStep => "guard_step",
            Self::QuickStand => "quick_stand",
            Self::RecoveryRoll => "recovery_roll",
            Self::LandingRecovery => "landing_recovery",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TechniqueButton {
    A,
    B,
    AB,
    Grab,
    Dash,
    Jump,
    Item,
    Special,
    Ultimate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BeeSkillId {
    WorkerSwarm,
    HoneyGlob,
    HomingSting,
    UltimateSwarm,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PenguinSkillId {
    FishTorpedo,
    PopsicleBounce,
    SledWake,
    IceTrail,
    UltimateIceField,
    SnowmanDrop,
    SnowHillRamp,
    SnowSlopeRide,
    SnowfortCannon,
    SpringPeck,
    BodySlam,
    GlacierParade,
    SnowflakeShot,
    SnowflakeSwapShot,
    SnowflakeBurst,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChickSkillId {
    #[allow(dead_code)]
    ShellPeck,
    SunnyFlip,
    ShellScramble,
    #[allow(dead_code)]
    EggCupMortar,
    OrbitEgg,
    OrbitEggLaunch,
    UltimateEggBurst,
    EggplantRoll,
    #[allow(dead_code)]
    FreshEggDrop,
    FreshEggRide,
    #[allow(dead_code)]
    SunnySideSplash,
    #[allow(dead_code)]
    OmeletField,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TechniqueStatus {
    Grounded,
    Airborne,
    Any,
}

impl TechniqueStatus {
    pub fn allows(self, grounded: bool) -> bool {
        match self {
            Self::Grounded => grounded,
            Self::Airborne => !grounded,
            Self::Any => true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MovementLock {
    Grounded,
    Locked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MsTimingWindow {
    pub start_ms: u32,
    pub end_ms: Option<u32>,
}

impl MsTimingWindow {
    pub const fn closed(start_ms: u32, end_ms: u32) -> Self {
        Self {
            start_ms,
            end_ms: Some(end_ms),
        }
    }

    #[allow(dead_code)]
    pub const fn open_ended(start_ms: u32) -> Self {
        Self {
            start_ms,
            end_ms: None,
        }
    }

    pub fn contains_elapsed_secs(self, elapsed: f32) -> bool {
        self.contains_ms(elapsed_secs_to_ms(elapsed))
    }

    pub fn contains_ms(self, elapsed_ms: u32) -> bool {
        elapsed_ms >= self.start_ms && self.end_ms.map_or(true, |end| elapsed_ms <= end)
    }
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttackShapeId {
    CompactSlashLead,
    CompactSlashFollow,
    CompactSlashTight,
    LauncherRiser,
    BodyRoll,
    CompactThrust,
    DelayedRiser,
    SweepingArcWide,
    HookSweep,
    RisingColumn,
    FallingSpikeArc,
    AirFishShot,
    CatPounceCatch,
    UltimateCatch,
    UltimateScratchLeft,
    UltimateScratchRight,
    UltimateBomb,
    ShoulderLine,
    CatBodySkid,
    GroundSkid,
    PenguinSlopeBody,
    PenguinUltimateSlopeBody,
    PigBodyShove,
    PigBellyCrash,
    PigRollingPinLine,
    PigHamLob,
    PigHalfCircleSwing,
    PigMeatSlam,
    PigAirMeatSlam,
    PigUltimateGrab,
    CurvedLob,
    CounterArc,
    ProjectileBolt,
    TrapPlate,
    ShockwaveRing,
    HazardField,
    ItemLob,
    BombBurst,
    GrabCatch,
    DashShoulder,
    JumpKick,
    ItemMelee,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AttackShapeDef {
    pub id: AttackShapeId,
    pub range: f32,
    pub radius: f32,
    pub vertical_offset_scale: f32,
    pub parented: bool,
    pub curved: bool,
    pub effect_type: u8,
    pub path: &'static [[f32; 3]],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
pub enum AttackPayloadId {
    AsBeat1,
    AsBeat2,
    AssBeat1,
    AssBeat2,
    KiriageBeat1,
    KiriageBeat2,
    HeavyStep,
    ComboFinisherLift,
    ComboFinisher,
    DashComboFinisher,
    PigSnoutShove,
    PigBellyBump,
    PigHamSlam,
    PigRollingPinStep,
    PigHamLauncher,
    PigHamSwingTap,
    PigHamSwingPartial,
    PigHamSwingFull,
    PigAirMeatSlam,
    DogBite1,
    DogBite2,
    DogBodyPounce,
    DogShoulderStep,
    DogLaunchBite,
    FoxSwipe1,
    FoxSwipe2,
    FoxTailSweep,
    FoxSkitterStep,
    FoxFlipLaunch,
    PandaPalm1,
    PandaPalm2,
    PandaBodyDrop,
    PandaWeightShift,
    PandaRisingScoop,
    BeeNeedleTap,
    BeeCrossSting,
    BeeSpiralSting,
    BeePiercingStep,
    BeeHiveLauncher,
    BeeAirSting,
    BeeHiveDive,
    BeeWorkerSting,
    BeeHoneyGlob,
    BeeHoneyPuddle,
    BeeHomingSting,
    PenguinFishSlap1,
    PenguinFishSlap2,
    PenguinBellySlide,
    PenguinPanBonk,
    PenguinSledScoop,
    PenguinSlopeCrash,
    PenguinIceSlide,
    PenguinPopsiclePeck,
    PenguinFrozenFishDive,
    PenguinFishTorpedo,
    PenguinPopsicleBounce,
    PenguinSledWake,
    PenguinSnowflakeShard,
    PenguinSnowBoulder,
    PenguinSnowmanDrop,
    PenguinBodySlamShockwave,
    ChickShellChip,
    ChickFriedEggDisc,
    ChickEggCupMortar,
    ChickOrbitEgg,
    ChickOrbitEggLaunch,
    ChickFreshEggDrop,
    ChickEggplantRoll,
    ChickSunnySplash,
    ChickOmeletField,
    ChickShellScoot,
    ChickShellScramble,
    GrabCatch,
    DashStrike,
    DashShoulderBeat,
    JumpStrike,
    JumpSpike,
    JumpFishShot,
    PigJumpBellyDrop,
    PigHamLob,
    DogJumpPounce,
    DogJumpFishShot,
    FoxJumpSwipe,
    FoxJumpFishShot,
    PandaJumpDrop,
    PandaJumpFishShot,
    UltimateCatch,
    UltimateScratchLight,
    UltimateScratchHeavy,
    UltimateBomb,
    PigUltimateCatch,
    PigUltimateScratchLight,
    PigUltimateScratchHeavy,
    PigUltimateBomb,
    DogUltimateCatch,
    DogUltimateScratchLight,
    DogUltimateScratchHeavy,
    DogUltimateBomb,
    FoxUltimateCatch,
    FoxUltimateScratchLight,
    FoxUltimateScratchHeavy,
    FoxUltimateBomb,
    PandaUltimateCatch,
    PandaUltimateScratchLight,
    PandaUltimateScratchHeavy,
    PandaUltimateBomb,
    BeeUltimateCatch,
    BeeUltimateScratchLight,
    BeeUltimateScratchHeavy,
    BeeUltimateBomb,
    BeeUltimateSwarmTick,
    PenguinUltimateCatch,
    PenguinUltimateScratchLight,
    PenguinUltimateScratchHeavy,
    PenguinUltimateBomb,
    PenguinUltimateSlopeCrash,
    GuardCounter,
    SpecialProjectile,
    SpecialTrap,
    SpecialShockwave,
    SpecialHazard,
    ItemThrowLight,
    ItemThrowHeavy,
    BombBlast,
}

pub fn payload_is_jump_spike(payload_id: AttackPayloadId) -> bool {
    matches!(
        payload_id,
        AttackPayloadId::JumpSpike
            | AttackPayloadId::DogJumpPounce
            | AttackPayloadId::FoxJumpSwipe
            | AttackPayloadId::PandaJumpDrop
            | AttackPayloadId::PigJumpBellyDrop
            | AttackPayloadId::BeeHiveDive
            | AttackPayloadId::PenguinFrozenFishDive
    )
}

pub fn payload_is_jump_fish(payload_id: AttackPayloadId) -> bool {
    matches!(
        payload_id,
        AttackPayloadId::JumpFishShot
            | AttackPayloadId::DogJumpFishShot
            | AttackPayloadId::FoxJumpFishShot
            | AttackPayloadId::PandaJumpFishShot
            | AttackPayloadId::PigHamLob
    )
}

pub fn payload_is_ultimate_catch(payload_id: AttackPayloadId) -> bool {
    matches!(
        payload_id,
        AttackPayloadId::UltimateCatch
            | AttackPayloadId::DogUltimateCatch
            | AttackPayloadId::FoxUltimateCatch
            | AttackPayloadId::PandaUltimateCatch
            | AttackPayloadId::PigUltimateCatch
            | AttackPayloadId::BeeUltimateCatch
            | AttackPayloadId::PenguinUltimateCatch
    )
}

pub fn payload_is_ultimate_scratch(payload_id: AttackPayloadId) -> bool {
    matches!(
        payload_id,
        AttackPayloadId::UltimateScratchLight
            | AttackPayloadId::UltimateScratchHeavy
            | AttackPayloadId::DogUltimateScratchLight
            | AttackPayloadId::DogUltimateScratchHeavy
            | AttackPayloadId::FoxUltimateScratchLight
            | AttackPayloadId::FoxUltimateScratchHeavy
            | AttackPayloadId::PandaUltimateScratchLight
            | AttackPayloadId::PandaUltimateScratchHeavy
            | AttackPayloadId::PigUltimateScratchLight
            | AttackPayloadId::PigUltimateScratchHeavy
            | AttackPayloadId::BeeUltimateScratchLight
            | AttackPayloadId::BeeUltimateScratchHeavy
            | AttackPayloadId::PenguinUltimateScratchLight
            | AttackPayloadId::PenguinUltimateScratchHeavy
    )
}

pub fn payload_is_ultimate_bomb(payload_id: AttackPayloadId) -> bool {
    matches!(
        payload_id,
        AttackPayloadId::UltimateBomb
            | AttackPayloadId::DogUltimateBomb
            | AttackPayloadId::FoxUltimateBomb
            | AttackPayloadId::PandaUltimateBomb
            | AttackPayloadId::PigUltimateBomb
            | AttackPayloadId::BeeUltimateBomb
            | AttackPayloadId::PenguinUltimateBomb
            | AttackPayloadId::PenguinUltimateSlopeCrash
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DamageProfileId {
    Direct,
    BasicStrike,
    FollowupStrike,
    LauncherCommit,
    GroundBounce,
    GrabControl,
    DashBody,
    AerialSpike,
    CounterBlow,
    UltimateRush,
    ItemHeavy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DamageElement {
    Neutral,
    Strike,
    Launch,
    Shock,
    Wind,
    Earth,
    Hazard,
    Blast,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DamageElementAffinity {
    Neutral,
    Resistant,
    Weak,
    Absorbed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DamageTargetStatus {
    Standing,
    Guarding,
    Attacking,
    Airborne,
    Downed,
    Recovering,
    GuardBroken,
    Grabbed,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DamageDefenseMode {
    Normal { factor: f32 },
    Ignore,
    Fixed { factor: f32 },
    NoDamage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DamageCondition {
    Guarded,
    Unguarded,
    Airborne,
    Downed,
    CounterHit,
    LowHealth,
    WeakGuard,
    GuardBreak,
    ProjectileSource,
    ItemSource,
    HazardSource,
    HeavyImpact,
    HighPower,
    LethalRaw,
    Element(DamageElement),
    ElementAffinity(DamageElementAffinity),
    TargetStatus(DamageTargetStatus),
    AttackerEquipment(EquipmentKind),
    AttackerStyle(FighterStyleKind),
    DefenderEquipment(EquipmentKind),
    DefenderStyle(FighterStyleKind),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DamageReductionDef {
    pub condition: DamageCondition,
    pub factor: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DamageModifierDef {
    pub condition: DamageCondition,
    pub scale: f32,
    pub add: f32,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DamageTerminalKind {
    Normal,
    Nonlethal,
    NoHpLoss,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DamageTerminalDef {
    pub kind: DamageTerminalKind,
    pub ignore_time_ms: u32,
    pub score_scale: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DamageTerminalOverrideDef {
    pub condition: DamageCondition,
    pub terminal: DamageTerminalDef,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DamageSideEffectId {
    GuardPressure,
    CounterSurge,
    JuggleBonus,
    DownedProration,
    LethalPunctuation,
    HazardAttrition,
    ItemCrush,
    ElementBurst,
    AccessorySurge,
    StatusExploit,
    ElementResist,
    ElementWeakness,
    ElementAbsorb,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DamageSideEffectDef {
    pub condition: DamageCondition,
    pub id: DamageSideEffectId,
    pub cue: &'static str,
    pub invulnerability_ms: u32,
    pub stamina_delta: f32,
    pub hud_flash: f32,
    pub score_scale_add: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DamageProfileDef {
    pub id: DamageProfileId,
    pub defense_mode: DamageDefenseMode,
    pub reductions: &'static [DamageReductionDef],
    pub modifiers: &'static [DamageModifierDef],
    pub side_effects: &'static [DamageSideEffectDef],
    pub terminal: DamageTerminalDef,
    pub terminal_overrides: &'static [DamageTerminalOverrideDef],
    pub guard_stamina_scale: f32,
    pub minimum_damage: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AttackPayloadDef {
    pub id: AttackPayloadId,
    pub kind: AttackKind,
    pub shape_id: AttackShapeId,
    pub reaction_family: ReactionFamilyId,
    pub damage_profile: DamageProfileId,
    pub element: DamageElement,
    pub power: f32,
    pub str_scale: f32,
    pub time_ms: u32,
    pub damage: f32,
    pub knockback: f32,
    pub vertical_knockback: f32,
    pub guardable: bool,
    pub impact_cue: &'static str,
    pub hitstop_scale: f32,
    pub shake_scale: f32,
    pub feedback_priority_bonus: u8,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
pub enum FeedbackPhase {
    Startup,
    PreHit,
    Impact,
    Aftermath,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MoveTimelineEventKind {
    Attack(AttackPayloadId),
    ChargedAttack {
        tap: AttackPayloadId,
        partial: AttackPayloadId,
        full: AttackPayloadId,
    },
    SpawnBeeSkill(BeeSkillId),
    SpawnPenguinSkill(PenguinSkillId),
    SpawnChickSkill(ChickSkillId),
    Feedback(FeedbackPhase, &'static str),
    Motion {
        forward: f32,
        lift: f32,
    },
    NextTech,
    Recover,
    Stop,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MoveTimelineEvent {
    pub at_ms: u32,
    pub kind: MoveTimelineEventKind,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MoveScriptDef {
    pub id: &'static str,
    pub animation_recovery_ms: Option<u32>,
    pub next_tech_ms: Option<u32>,
    pub recover_ms: u32,
    pub events: &'static [MoveTimelineEvent],
}

impl MoveScriptDef {
    pub fn duration_secs(self) -> f32 {
        ms_to_secs(self.recover_ms)
    }

    pub fn next_tech_open(self, elapsed: f32) -> bool {
        self.next_tech_ms
            .is_some_and(|next_ms| elapsed_secs_to_ms(elapsed) >= next_ms)
    }

    pub fn recovered(self, elapsed: f32) -> bool {
        elapsed_secs_to_ms(elapsed) >= self.recover_ms
    }
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TechniquePredicate {
    Button(TechniqueButton),
    Character(CharacterKind),
    Style(FighterStyleKind),
    Equipment(EquipmentKind),
    LoadoutTag(LoadoutTag),
    Grounded,
    Airborne,
    ConfirmedHit,
    Whiffed,
    CancelWindowOpen,
    BranchWindowOpen,
    CurrentAction(FighterAction),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TechniqueMatchContext {
    pub previous: Option<TechniqueId>,
    pub button: TechniqueButton,
    pub elapsed: f32,
    pub style: FighterStyleKind,
    pub loadout: LoadoutContext,
    pub grounded: bool,
    pub confirmed_hit: bool,
    pub cancel_window_open: bool,
    pub branch_window_open: bool,
    pub current_action: FighterAction,
}

impl TechniquePredicate {
    pub fn matches(self, context: TechniqueMatchContext) -> bool {
        match self {
            Self::Button(button) => context.button == button,
            Self::Character(character) => context.loadout.character == character,
            Self::Style(style) => context.style == style,
            Self::Equipment(equipment) => context.loadout.equipment == Some(equipment),
            Self::LoadoutTag(tag) => loadout_has_tag(context.loadout, tag),
            Self::Grounded => context.grounded,
            Self::Airborne => !context.grounded,
            Self::ConfirmedHit => context.confirmed_hit,
            Self::Whiffed => !context.confirmed_hit,
            Self::CancelWindowOpen => context.cancel_window_open,
            Self::BranchWindowOpen => context.branch_window_open,
            Self::CurrentAction(action) => context.current_action == action,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TechniquePredicateSet {
    pub all: &'static [TechniquePredicate],
    pub any: &'static [TechniquePredicate],
    pub none: &'static [TechniquePredicate],
}

impl TechniquePredicateSet {
    pub fn matches(self, context: TechniqueMatchContext) -> bool {
        self.all.iter().all(|predicate| predicate.matches(context))
            && (self.any.is_empty() || self.any.iter().any(|predicate| predicate.matches(context)))
            && self
                .none
                .iter()
                .all(|predicate| !predicate.matches(context))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrevTechExpr {
    pub any_of: &'static [TechniqueId],
    pub command: Option<TechniqueButton>,
    pub conditions: TechniquePredicateSet,
}

impl PrevTechExpr {
    pub fn matches(self, context: TechniqueMatchContext) -> bool {
        let Some(previous) = context.previous else {
            return false;
        };
        self.any_of.contains(&previous)
            && self
                .command
                .map_or(true, |command| command == context.button)
            && self.conditions.matches(context)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TechniqueChainRule {
    pub previous: PrevTechExpr,
    pub window: MsTimingWindow,
    pub same_button_required: bool,
}

impl TechniqueChainRule {
    pub fn matches(self, context: TechniqueMatchContext) -> bool {
        self.previous.matches(context) && self.window.contains_elapsed_secs(context.elapsed)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TechniqueDefinition {
    pub id: TechniqueId,
    pub action: FighterAction,
    pub button: TechniqueButton,
    pub status: TechniqueStatus,
    pub script: MoveScriptDef,
    pub input_buffer_ms: u32,
    pub stamina_cost: f32,
    pub movement_lock: MovementLock,
    pub cancel_window: Option<MsTimingWindow>,
    pub branch_window: Option<MsTimingWindow>,
    pub chain_rule: Option<TechniqueChainRule>,
}

impl TechniqueDefinition {
    pub fn duration(self) -> f32 {
        self.script.duration_secs()
    }

    #[allow(dead_code)]
    pub fn duration_ms(self) -> u32 {
        self.script.recover_ms
    }

    pub fn cancel_open(self, elapsed: f32) -> bool {
        self.cancel_window
            .is_some_and(|window| window.contains_elapsed_secs(elapsed))
    }

    pub fn branch_open(self, elapsed: f32) -> bool {
        self.branch_window
            .is_some_and(|window| window.contains_elapsed_secs(elapsed))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TechniqueRuntime {
    pub id: Option<TechniqueId>,
    pub cancel_open: bool,
    pub branch_open: bool,
    pub next_tech_open: bool,
    pub recovered: bool,
}

pub const PIG_HEAVY_PARTIAL_CHARGE_MS: u32 = 420;
pub const PIG_HEAVY_FULL_CHARGE_MS: u32 = 900;
pub const PIG_HEAVY_ATTACK_MS: u32 = 360;

pub fn charged_payload_for_elapsed(
    charge_elapsed: f32,
    tap: AttackPayloadId,
    partial: AttackPayloadId,
    full: AttackPayloadId,
) -> AttackPayloadId {
    let charge_ms = elapsed_secs_to_ms(charge_elapsed);
    if charge_ms >= PIG_HEAVY_FULL_CHARGE_MS {
        full
    } else if charge_ms >= PIG_HEAVY_PARTIAL_CHARGE_MS {
        partial
    } else {
        tap
    }
}

const A_S_PREV: &[TechniqueId] = &[TechniqueId::CatLight1];
const A_SS_PREV: &[TechniqueId] = &[TechniqueId::CatLight2];
const X_STEP_PREV: &[TechniqueId] = &[TechniqueId::CatHeavy];
const PIG_A_S_PREV: &[TechniqueId] = &[TechniqueId::PigLight1];
const PIG_A_SS_PREV: &[TechniqueId] = &[TechniqueId::PigLight2];
const PIG_X_STEP_PREV: &[TechniqueId] = &[TechniqueId::PigHeavy];
const DOG_A_S_PREV: &[TechniqueId] = &[TechniqueId::DogLight1];
const DOG_A_SS_PREV: &[TechniqueId] = &[TechniqueId::DogLight2];
const DOG_X_STEP_PREV: &[TechniqueId] = &[TechniqueId::DogHeavy];
const FOX_A_S_PREV: &[TechniqueId] = &[TechniqueId::FoxLight1];
const FOX_A_SS_PREV: &[TechniqueId] = &[TechniqueId::FoxLight2];
const FOX_X_STEP_PREV: &[TechniqueId] = &[TechniqueId::FoxHeavy];
const PANDA_A_S_PREV: &[TechniqueId] = &[TechniqueId::PandaLight1];
const PANDA_A_SS_PREV: &[TechniqueId] = &[TechniqueId::PandaLight2];
const PANDA_X_STEP_PREV: &[TechniqueId] = &[TechniqueId::PandaHeavy];
const BEE_A_S_PREV: &[TechniqueId] = &[TechniqueId::BeeLight1];
const BEE_A_SS_PREV: &[TechniqueId] = &[TechniqueId::BeeLight2];
const PENGUIN_A_S_PREV: &[TechniqueId] = &[TechniqueId::PenguinLight1];
const PENGUIN_X_STEP_PREV: &[TechniqueId] = &[];
const CHICK_A_S_PREV: &[TechniqueId] = &[TechniqueId::ChickLight1];
const CHICK_A_SS_PREV: &[TechniqueId] = &[TechniqueId::ChickLight2];
const CHICK_X_STEP_PREV: &[TechniqueId] = &[TechniqueId::ChickHeavy];
const NO_TECHNIQUE_PREDICATES: &[TechniquePredicate] = &[];
const A_CHAIN_ALL_PREDICATES: &[TechniquePredicate] = &[
    TechniquePredicate::Button(TechniqueButton::A),
    TechniquePredicate::Grounded,
    TechniquePredicate::BranchWindowOpen,
];
const A_CHAIN_NONE_PREDICATES: &[TechniquePredicate] = &[TechniquePredicate::CurrentAction(
    FighterAction::HeavyAttack,
)];
const A_CHAIN_CONDITIONS: TechniquePredicateSet = TechniquePredicateSet {
    all: A_CHAIN_ALL_PREDICATES,
    any: NO_TECHNIQUE_PREDICATES,
    none: A_CHAIN_NONE_PREDICATES,
};
const A_FINISHER_ALL_PREDICATES: &[TechniquePredicate] = &[
    TechniquePredicate::Button(TechniqueButton::A),
    TechniquePredicate::Grounded,
    TechniquePredicate::BranchWindowOpen,
    TechniquePredicate::CurrentAction(FighterAction::LightAttack2),
];
const A_FINISHER_ANY_PREDICATES: &[TechniquePredicate] = &[];
const A_FINISHER_NONE_PREDICATES: &[TechniquePredicate] = &[
    TechniquePredicate::Airborne,
    TechniquePredicate::CurrentAction(FighterAction::HeavyAttack),
];
const A_FINISHER_CONDITIONS: TechniquePredicateSet = TechniquePredicateSet {
    all: A_FINISHER_ALL_PREDICATES,
    any: A_FINISHER_ANY_PREDICATES,
    none: A_FINISHER_NONE_PREDICATES,
};
const X_CHAIN_ALL_PREDICATES: &[TechniquePredicate] = &[
    TechniquePredicate::Button(TechniqueButton::B),
    TechniquePredicate::Grounded,
    TechniquePredicate::BranchWindowOpen,
    TechniquePredicate::CurrentAction(FighterAction::HeavyAttack),
];
const X_CHAIN_CONDITIONS: TechniquePredicateSet = TechniquePredicateSet {
    all: X_CHAIN_ALL_PREDICATES,
    any: NO_TECHNIQUE_PREDICATES,
    none: NO_TECHNIQUE_PREDICATES,
};
const COMPACT_HOOK_EDGE_RANGE: f32 = FIGHTER_RADIUS * 1.55;
const COMPACT_HOOK_EDGE_RADIUS: f32 = FIGHTER_RADIUS * 0.32;
const COMPACT_SLASH_LEAD_PATH: [[f32; 3]; 3] =
    [[-0.18, 0.0, 0.02], [-0.04, 0.0, 0.1], [0.14, 0.0, 0.16]];
const COMPACT_SLASH_FOLLOW_PATH: [[f32; 3]; 3] =
    [[0.18, 0.0, 0.02], [0.04, 0.0, 0.1], [-0.14, 0.0, 0.16]];
const COMPACT_SLASH_TIGHT_PATH: [[f32; 3]; 3] =
    [[0.0, 0.0, -0.2], [0.0, 0.0, 0.0], [0.0, 0.0, 0.16]];
const BODY_ROLL_PATH: [[f32; 3]; 1] = [[0.0, 0.0, 0.0]];
const COMPACT_THRUST_PATH: [[f32; 3]; 2] = [[0.0, 0.0, -0.2], [0.0, 0.0, 0.0]];
const LAUNCHER_RISER_PATH: [[f32; 3]; 4] = [
    [0.0, 0.0, 0.0],
    [0.0, 0.75, 0.8],
    [0.0, 1.45, 1.35],
    [0.0, 2.0, 1.7],
];
const DELAYED_RISER_PATH: [[f32; 3]; 11] = [
    [0.0, 0.0, 0.0],
    [0.0, 0.0, 0.0],
    [0.0, 0.0, 0.0],
    [0.0, 0.0, 0.0],
    [0.0, 0.12, 0.24],
    [0.0, 0.34, 0.58],
    [0.0, 0.68, 1.05],
    [0.0, 1.05, 1.62],
    [0.0, 1.42, 2.28],
    [0.0, 1.74, 2.92],
    [0.0, 2.0, 3.46],
];
const SWEEPING_ARC_WIDE_PATH: [[f32; 3]; 5] = [
    [-0.42, 0.0, -0.18],
    [-0.28, 0.0, 0.08],
    [0.0, 0.0, 0.24],
    [0.32, 0.0, 0.16],
    [0.48, 0.0, -0.04],
];
const HOOK_SWEEP_PATH: [[f32; 3]; 6] = [
    [0.42, 0.0, -0.18],
    [0.28, 0.0, 0.04],
    [0.0, 0.0, 0.24],
    [-0.32, 0.0, 0.18],
    [-0.44, 0.0, -0.04],
    [-0.18, 0.0, -0.16],
];
const RISING_COLUMN_PATH: [[f32; 3]; 8] = [
    [0.0, 0.0, 0.1],
    [0.0, 0.22, 0.28],
    [0.0, 0.54, 0.5],
    [0.0, 0.92, 0.78],
    [0.0, 1.28, 1.0],
    [0.0, 1.56, 1.12],
    [0.0, 1.78, 0.98],
    [0.0, 1.9, 0.74],
];
const FALLING_SPIKE_ARC_PATH: [[f32; 3]; 6] = [
    [0.0, 0.26, -0.18],
    [0.0, 0.18, 0.02],
    [0.0, 0.04, 0.24],
    [0.0, -0.18, 0.38],
    [0.0, -0.32, 0.26],
    [0.0, -0.24, 0.04],
];
const AIR_FISH_SHOT_PATH: [[f32; 3]; 7] = [
    [0.0, 0.18, -0.04],
    [0.0, 0.14, 0.488],
    [0.0, 0.06, 1.08],
    [0.0, -0.05, 1.736],
    [0.0, -0.18, 2.408],
    [0.0, -0.32, 3.048],
    [0.0, -0.46, 3.64],
];
const CAT_POUNCE_CATCH_RANGE: f32 = FIGHTER_RADIUS * 0.72;
const CAT_POUNCE_CATCH_RADIUS: f32 = FIGHTER_RADIUS * 0.9;
const CAT_POUNCE_CATCH_PATH: [[f32; 3]; 5] = [
    [0.0, -0.04, -0.12],
    [0.0, -0.02, -0.04],
    [0.0, 0.02, 0.08],
    [0.0, 0.0, 0.16],
    [0.0, -0.04, 0.06],
];
const ULTIMATE_CATCH_PATH: [[f32; 3]; 3] =
    [[0.0, 0.04, -0.08], [0.0, 0.06, 0.24], [0.0, 0.04, 0.48]];
const ULTIMATE_SCRATCH_LEFT_PATH: [[f32; 3]; 5] = [
    [-0.36, 0.06, -0.08],
    [-0.2, 0.08, 0.16],
    [0.08, 0.06, 0.34],
    [0.32, 0.03, 0.22],
    [0.18, 0.0, -0.04],
];
const ULTIMATE_SCRATCH_RIGHT_PATH: [[f32; 3]; 5] = [
    [0.36, 0.06, -0.08],
    [0.2, 0.08, 0.16],
    [-0.08, 0.06, 0.34],
    [-0.32, 0.03, 0.22],
    [-0.18, 0.0, -0.04],
];
const ULTIMATE_BOMB_PATH: [[f32; 3]; 5] = [
    [0.0, 0.18, 0.0],
    [0.0, 0.08, 0.22],
    [0.0, -0.08, 0.38],
    [0.0, -0.16, 0.22],
    [0.0, -0.08, 0.0],
];
const SHOULDER_LINE_PATH: [[f32; 3]; 5] = [
    [0.0, 0.0, -0.2],
    [0.0, 0.0, 0.16],
    [0.0, 0.0, 0.56],
    [0.0, 0.0, 0.92],
    [0.0, 0.0, 0.68],
];
const CAT_BODY_SKID_RANGE: f32 = FIGHTER_RADIUS * 0.9;
const CAT_BODY_SKID_RADIUS: f32 = FIGHTER_RADIUS * 0.86;
const CAT_BODY_SKID_PATH: [[f32; 3]; 5] = [
    [0.0, -0.08, -0.08],
    [0.0, -0.12, -0.02],
    [0.0, -0.16, 0.06],
    [0.0, -0.14, 0.12],
    [0.0, -0.08, 0.02],
];
const GROUND_SKID_BODY_RANGE: f32 = FIGHTER_RADIUS * 1.25;
const GROUND_SKID_BODY_RADIUS: f32 = COMBO_FINISHER_RADIUS * 0.72;
const GROUND_SKID_PATH: [[f32; 3]; 5] = [
    [0.0, -0.08, 0.0],
    [0.0, -0.12, 0.18],
    [0.0, -0.16, 0.42],
    [0.0, -0.14, 0.68],
    [0.0, -0.08, 0.52],
];
const PENGUIN_SLOPE_BODY_RANGE: f32 = FIGHTER_RADIUS * 0.75;
const PENGUIN_SLOPE_BODY_RADIUS: f32 = FIGHTER_RADIUS * 0.86;
const PENGUIN_ULTIMATE_SLOPE_BODY_RADIUS: f32 = FIGHTER_RADIUS * 1.35;
const PENGUIN_ULTIMATE_SLOPE_BODY_RANGE: f32 = PENGUIN_SLOPE_BODY_RANGE;
const PENGUIN_SLOPE_BODY_PATH: [[f32; 3]; 5] = [
    [0.0, -0.08, -0.02],
    [0.0, -0.11, 0.0],
    [0.0, -0.13, 0.02],
    [0.0, -0.10, 0.04],
    [0.0, -0.07, 0.0],
];
const PENGUIN_SLOPE_LAUNCH_FORWARD: f32 = 7.4;
const PENGUIN_SLOPE_EXIT_FORWARD: f32 = 3.4;
pub const PENGUIN_SLOPE_TOTAL_FORWARD: f32 =
    PENGUIN_SLOPE_LAUNCH_FORWARD + PENGUIN_SLOPE_EXIT_FORWARD;
const PENGUIN_SLOPE_ULTIMATE_HITSTOP_SCALE: f32 = 0.0;
const PIG_BODY_SHOVE_PATH: [[f32; 3]; 4] = [
    [0.0, -0.04, -0.14],
    [0.0, -0.05, 0.06],
    [0.0, -0.06, 0.24],
    [0.0, -0.03, 0.1],
];
const PIG_BELLY_CRASH_PATH: [[f32; 3]; 5] = [
    [-0.18, -0.08, -0.08],
    [-0.05, -0.1, 0.12],
    [0.0, -0.12, 0.36],
    [0.18, -0.08, 0.2],
    [0.0, -0.05, -0.02],
];
const PIG_ROLLING_PIN_LINE_PATH: [[f32; 3]; 6] = [
    [0.0, 0.0, -0.18],
    [0.0, 0.0, 0.12],
    [0.0, 0.0, 0.48],
    [0.0, 0.0, 0.84],
    [0.0, 0.0, 1.04],
    [0.0, 0.0, 0.7],
];
const PIG_HAM_LOB_PATH: [[f32; 3]; 6] = [
    [0.0, 0.12, -0.08],
    [0.0, 0.22, 0.36],
    [0.0, 0.18, 0.92],
    [0.0, 0.02, 1.42],
    [0.0, -0.26, 1.84],
    [0.0, -0.52, 2.12],
];
const PIG_HALF_CIRCLE_SWING_PATH: [[f32; 3]; 7] = [
    [-0.72, 0.1, -0.22],
    [-0.52, 0.08, 0.08],
    [-0.22, 0.04, 0.34],
    [0.12, 0.0, 0.46],
    [0.46, -0.03, 0.26],
    [0.68, -0.04, -0.04],
    [0.42, -0.02, -0.2],
];
const PIG_MEAT_SLAM_PATH: [[f32; 3]; 6] = [
    [-0.16, 0.18, -0.12],
    [-0.08, 0.1, 0.12],
    [0.0, 0.0, 0.36],
    [0.08, -0.1, 0.58],
    [0.02, -0.16, 0.82],
    [0.0, -0.1, 0.58],
];
const PIG_AIR_MEAT_SLAM_PATH: [[f32; 3]; 6] = [
    [0.0, 0.92, -0.24],
    [0.0, 0.86, 0.0],
    [0.0, 0.52, 0.28],
    [0.0, 0.08, 0.52],
    [0.0, -0.36, 0.62],
    [0.0, -0.74, 0.48],
];
const PIG_ULTIMATE_GRAB_PATH: [[f32; 3]; 4] = [
    [0.0, 0.04, -0.08],
    [0.0, 0.04, 0.18],
    [0.0, 0.02, 0.46],
    [0.0, 0.0, 0.28],
];
const CURVED_LOB_PATH: [[f32; 3]; 3] = [[0.0, 0.0, 0.0], [0.0, 3.0, 0.5], [0.0, 0.0, 1.0]];
const COUNTER_ARC_PATH: [[f32; 3]; 4] = [
    [-0.18, 0.0, -0.12],
    [-0.04, 0.06, 0.08],
    [0.14, 0.03, 0.16],
    [0.04, 0.0, -0.02],
];
const PROJECTILE_BOLT_PATH: [[f32; 3]; 3] =
    [[0.0, 0.0, -0.08], [0.0, 0.02, 0.12], [0.0, 0.0, 0.36]];
const TRAP_PLATE_PATH: [[f32; 3]; 4] = [
    [0.0, 0.0, 0.0],
    [-0.18, 0.0, 0.02],
    [0.18, 0.0, 0.02],
    [0.0, 0.0, 0.0],
];
const SHOCKWAVE_RING_PATH: [[f32; 3]; 5] = [
    [0.0, 0.0, 0.0],
    [0.0, 0.0, 0.42],
    [0.0, 0.0, 0.95],
    [0.0, 0.0, 1.6],
    [0.0, 0.0, 2.35],
];
const HAZARD_FIELD_PATH: [[f32; 3]; 4] = [
    [0.0, 0.0, 0.0],
    [0.2, 0.0, 0.12],
    [-0.2, 0.0, 0.16],
    [0.0, 0.0, 0.0],
];
const ITEM_LOB_PATH: [[f32; 3]; 3] = [[0.0, 0.0, 0.0], [0.0, 1.05, 0.35], [0.0, 0.0, 0.82]];
const BOMB_BURST_PATH: [[f32; 3]; 4] = [
    [0.0, 0.0, 0.0],
    [0.0, 0.12, 0.7],
    [0.0, 0.08, 1.55],
    [0.0, 0.0, 2.4],
];
const GRAB_CATCH_PATH: [[f32; 3]; 1] = [[0.0, 0.0, 0.0]];
const DASH_SHOULDER_PATH: [[f32; 3]; 4] = [
    [0.0, 0.0, -0.18],
    [0.0, 0.0, 0.18],
    [0.0, 0.0, 0.46],
    [0.0, 0.0, 0.3],
];
const JUMP_KICK_PATH: [[f32; 3]; 4] = [
    [0.0, 0.12, -0.16],
    [0.0, 0.02, 0.12],
    [0.0, -0.12, 0.34],
    [0.0, -0.22, 0.18],
];
const ITEM_MELEE_PATH: [[f32; 3]; 2] = [[0.0, 0.0, -0.1], [0.0, 0.0, 0.35]];

const NO_DAMAGE_REDUCTIONS: &[DamageReductionDef] = &[];
const NO_DAMAGE_MODIFIERS: &[DamageModifierDef] = &[];
const COMMITTED_MOVE_REDUCTIONS: &[DamageReductionDef] = &[
    damage_reduction(DamageCondition::Downed, 0.4),
    damage_reduction(DamageCondition::GuardBreak, 0.85),
];
const BOUNCE_MOVE_REDUCTIONS: &[DamageReductionDef] = &[
    damage_reduction(DamageCondition::Downed, 0.45),
    damage_reduction(DamageCondition::Airborne, 0.92),
];

const NO_DAMAGE_TERMINAL_OVERRIDES: &[DamageTerminalOverrideDef] = &[];
const ELEMENT_ABSORB_TERMINALS: &[DamageTerminalOverrideDef] = &[damage_terminal_override(
    DamageCondition::ElementAffinity(DamageElementAffinity::Absorbed),
    DamageTerminalKind::NoHpLoss,
    240,
    0.0,
)];

const DIRECT_DAMAGE_MODIFIERS: &[DamageModifierDef] = &[
    damage_modifier(DamageCondition::Unguarded, 1.0, 0.0),
    damage_modifier(DamageCondition::Guarded, 0.25, 0.0),
    damage_modifier(DamageCondition::Downed, 0.35, 0.0),
    damage_modifier(
        DamageCondition::TargetStatus(DamageTargetStatus::Recovering),
        0.72,
        0.0,
    ),
    damage_modifier(DamageCondition::ProjectileSource, 0.92, 0.0),
    damage_modifier(DamageCondition::HazardSource, 0.85, 0.0),
    damage_modifier(DamageCondition::Element(DamageElement::Hazard), 0.92, 1.0),
    damage_modifier(
        DamageCondition::ElementAffinity(DamageElementAffinity::Resistant),
        0.84,
        0.0,
    ),
    damage_modifier(
        DamageCondition::ElementAffinity(DamageElementAffinity::Weak),
        1.14,
        0.0,
    ),
    damage_modifier(
        DamageCondition::ElementAffinity(DamageElementAffinity::Absorbed),
        0.0,
        0.0,
    ),
    damage_modifier(
        DamageCondition::AttackerStyle(FighterStyleKind::Catalyst),
        1.04,
        0.0,
    ),
];
const BASIC_STRIKE_DAMAGE_MODIFIERS: &[DamageModifierDef] = &[
    damage_modifier(DamageCondition::Guarded, 0.24, 0.0),
    damage_modifier(DamageCondition::Airborne, 0.9, 0.0),
    damage_modifier(DamageCondition::Downed, 0.3, 0.0),
    damage_modifier(DamageCondition::CounterHit, 1.1, 0.0),
    damage_modifier(DamageCondition::WeakGuard, 1.2, 0.0),
    damage_modifier(DamageCondition::Element(DamageElement::Shock), 1.06, 0.0),
    damage_modifier(
        DamageCondition::TargetStatus(DamageTargetStatus::GuardBroken),
        1.12,
        0.0,
    ),
    damage_modifier(
        DamageCondition::ElementAffinity(DamageElementAffinity::Resistant),
        0.88,
        0.0,
    ),
    damage_modifier(
        DamageCondition::ElementAffinity(DamageElementAffinity::Weak),
        1.12,
        0.0,
    ),
    damage_modifier(
        DamageCondition::ElementAffinity(DamageElementAffinity::Absorbed),
        0.0,
        0.0,
    ),
    damage_modifier(
        DamageCondition::AttackerStyle(FighterStyleKind::Anchor),
        1.02,
        0.0,
    ),
];
const FOLLOWUP_DAMAGE_MODIFIERS: &[DamageModifierDef] = &[
    damage_modifier(DamageCondition::Guarded, 0.22, 0.0),
    damage_modifier(DamageCondition::Airborne, 0.96, 0.0),
    damage_modifier(DamageCondition::Downed, 0.25, 0.0),
    damage_modifier(DamageCondition::CounterHit, 1.15, 0.0),
    damage_modifier(DamageCondition::WeakGuard, 1.18, 0.0),
    damage_modifier(DamageCondition::Element(DamageElement::Strike), 1.02, 0.0),
    damage_modifier(
        DamageCondition::TargetStatus(DamageTargetStatus::Recovering),
        0.82,
        0.0,
    ),
    damage_modifier(
        DamageCondition::ElementAffinity(DamageElementAffinity::Resistant),
        0.86,
        0.0,
    ),
    damage_modifier(
        DamageCondition::ElementAffinity(DamageElementAffinity::Weak),
        1.15,
        0.0,
    ),
    damage_modifier(
        DamageCondition::ElementAffinity(DamageElementAffinity::Absorbed),
        0.0,
        0.0,
    ),
];
const LAUNCHER_DAMAGE_MODIFIERS: &[DamageModifierDef] = &[
    damage_modifier(DamageCondition::Guarded, 0.16, 0.0),
    damage_modifier(DamageCondition::Airborne, 1.18, 0.0),
    damage_modifier(DamageCondition::Downed, 0.2, 0.0),
    damage_modifier(DamageCondition::CounterHit, 1.25, 0.0),
    damage_modifier(DamageCondition::HighPower, 1.05, 0.0),
    damage_modifier(DamageCondition::GuardBreak, 1.15, 0.0),
    damage_modifier(DamageCondition::Element(DamageElement::Launch), 1.06, 0.0),
    damage_modifier(DamageCondition::Element(DamageElement::Blast), 1.12, 0.0),
    damage_modifier(
        DamageCondition::DefenderStyle(FighterStyleKind::Anchor),
        0.95,
        0.0,
    ),
    damage_modifier(
        DamageCondition::ElementAffinity(DamageElementAffinity::Resistant),
        0.84,
        0.0,
    ),
    damage_modifier(
        DamageCondition::ElementAffinity(DamageElementAffinity::Weak),
        1.2,
        0.0,
    ),
    damage_modifier(
        DamageCondition::ElementAffinity(DamageElementAffinity::Absorbed),
        0.0,
        0.0,
    ),
    damage_modifier(
        DamageCondition::AttackerEquipment(EquipmentKind::HeavySeal),
        1.08,
        0.0,
    ),
    damage_modifier(
        DamageCondition::AttackerStyle(FighterStyleKind::Anchor),
        1.04,
        0.0,
    ),
];
const GROUND_BOUNCE_DAMAGE_MODIFIERS: &[DamageModifierDef] = &[
    damage_modifier(DamageCondition::Guarded, 0.18, 0.0),
    damage_modifier(DamageCondition::Airborne, 1.14, 0.0),
    damage_modifier(DamageCondition::Downed, 0.4, 0.0),
    damage_modifier(DamageCondition::CounterHit, 1.2, 0.0),
    damage_modifier(DamageCondition::LethalRaw, 1.08, 0.0),
    damage_modifier(DamageCondition::Element(DamageElement::Earth), 1.05, 0.0),
    damage_modifier(
        DamageCondition::ElementAffinity(DamageElementAffinity::Resistant),
        0.82,
        0.0,
    ),
    damage_modifier(
        DamageCondition::ElementAffinity(DamageElementAffinity::Weak),
        1.18,
        0.0,
    ),
    damage_modifier(
        DamageCondition::ElementAffinity(DamageElementAffinity::Absorbed),
        0.0,
        0.0,
    ),
    damage_modifier(
        DamageCondition::AttackerStyle(FighterStyleKind::Catalyst),
        1.03,
        0.0,
    ),
];
const GRAB_DAMAGE_MODIFIERS: &[DamageModifierDef] = &[
    damage_modifier(DamageCondition::Airborne, 0.85, 0.0),
    damage_modifier(DamageCondition::Downed, 0.5, 0.0),
    damage_modifier(DamageCondition::HeavyImpact, 1.08, 0.0),
    damage_modifier(
        DamageCondition::TargetStatus(DamageTargetStatus::Grabbed),
        0.76,
        0.0,
    ),
];
const DASH_BODY_DAMAGE_MODIFIERS: &[DamageModifierDef] = &[
    damage_modifier(DamageCondition::Guarded, 0.2, 0.0),
    damage_modifier(DamageCondition::Airborne, 1.05, 0.0),
    damage_modifier(DamageCondition::Downed, 0.25, 0.0),
    damage_modifier(DamageCondition::CounterHit, 1.22, 0.0),
    damage_modifier(DamageCondition::WeakGuard, 1.15, 0.0),
    damage_modifier(DamageCondition::Element(DamageElement::Wind), 1.06, 1.0),
    damage_modifier(
        DamageCondition::ElementAffinity(DamageElementAffinity::Resistant),
        0.84,
        0.0,
    ),
    damage_modifier(
        DamageCondition::ElementAffinity(DamageElementAffinity::Weak),
        1.2,
        0.0,
    ),
    damage_modifier(
        DamageCondition::ElementAffinity(DamageElementAffinity::Absorbed),
        0.0,
        0.0,
    ),
    damage_modifier(
        DamageCondition::AttackerEquipment(EquipmentKind::DashCoil),
        1.08,
        1.0,
    ),
    damage_modifier(
        DamageCondition::AttackerStyle(FighterStyleKind::Vector),
        1.05,
        1.0,
    ),
];
const AERIAL_SPIKE_DAMAGE_MODIFIERS: &[DamageModifierDef] = &[
    damage_modifier(DamageCondition::Guarded, 0.18, 0.0),
    damage_modifier(DamageCondition::Airborne, 1.35, 0.0),
    damage_modifier(DamageCondition::Downed, 0.5, 0.0),
    damage_modifier(DamageCondition::CounterHit, 1.12, 0.0),
    damage_modifier(DamageCondition::LethalRaw, 1.05, 0.0),
    damage_modifier(DamageCondition::Element(DamageElement::Wind), 1.07, 0.0),
    damage_modifier(
        DamageCondition::ElementAffinity(DamageElementAffinity::Resistant),
        0.82,
        0.0,
    ),
    damage_modifier(
        DamageCondition::ElementAffinity(DamageElementAffinity::Weak),
        1.18,
        0.0,
    ),
    damage_modifier(
        DamageCondition::ElementAffinity(DamageElementAffinity::Absorbed),
        0.0,
        0.0,
    ),
    damage_modifier(
        DamageCondition::AttackerEquipment(EquipmentKind::AerialSpur),
        1.08,
        0.0,
    ),
];
const COUNTER_BLOW_DAMAGE_MODIFIERS: &[DamageModifierDef] = &[
    damage_modifier(DamageCondition::Guarded, 0.1, 0.0),
    damage_modifier(DamageCondition::Airborne, 1.1, 0.0),
    damage_modifier(DamageCondition::Downed, 0.25, 0.0),
    damage_modifier(DamageCondition::CounterHit, 1.35, 0.0),
    damage_modifier(DamageCondition::LowHealth, 1.1, 0.0),
    damage_modifier(DamageCondition::GuardBreak, 1.25, 0.0),
    damage_modifier(DamageCondition::Element(DamageElement::Shock), 1.12, 0.0),
    damage_modifier(
        DamageCondition::DefenderEquipment(EquipmentKind::CounterCell),
        0.92,
        0.0,
    ),
    damage_modifier(
        DamageCondition::ElementAffinity(DamageElementAffinity::Resistant),
        0.82,
        0.0,
    ),
    damage_modifier(
        DamageCondition::ElementAffinity(DamageElementAffinity::Weak),
        1.18,
        0.0,
    ),
    damage_modifier(
        DamageCondition::ElementAffinity(DamageElementAffinity::Absorbed),
        0.0,
        0.0,
    ),
    damage_modifier(
        DamageCondition::AttackerEquipment(EquipmentKind::CounterCell),
        1.12,
        0.0,
    ),
];
const ITEM_HEAVY_DAMAGE_MODIFIERS: &[DamageModifierDef] = &[
    damage_modifier(DamageCondition::Guarded, 0.3, 0.0),
    damage_modifier(DamageCondition::Downed, 0.6, 0.0),
    damage_modifier(DamageCondition::CounterHit, 1.08, 0.0),
    damage_modifier(DamageCondition::ItemSource, 1.12, 0.0),
    damage_modifier(DamageCondition::LethalRaw, 1.1, 0.0),
    damage_modifier(DamageCondition::Element(DamageElement::Earth), 1.06, 0.0),
    damage_modifier(DamageCondition::Element(DamageElement::Blast), 1.12, 0.0),
    damage_modifier(
        DamageCondition::ElementAffinity(DamageElementAffinity::Resistant),
        0.86,
        0.0,
    ),
    damage_modifier(
        DamageCondition::ElementAffinity(DamageElementAffinity::Weak),
        1.16,
        0.0,
    ),
    damage_modifier(
        DamageCondition::ElementAffinity(DamageElementAffinity::Absorbed),
        0.0,
        0.0,
    ),
];

const NO_DAMAGE_SIDE_EFFECTS: &[DamageSideEffectDef] = &[];
const DIRECT_DAMAGE_SIDE_EFFECTS: &[DamageSideEffectDef] = &[
    damage_side_effect(
        DamageCondition::HazardSource,
        DamageSideEffectId::HazardAttrition,
        "damage_hazard_attrition",
        0,
        -2.0,
        0.08,
        0.0,
    ),
    damage_side_effect(
        DamageCondition::Element(DamageElement::Hazard),
        DamageSideEffectId::ElementBurst,
        "damage_element_hazard",
        80,
        -1.0,
        0.12,
        0.05,
    ),
    damage_side_effect(
        DamageCondition::ElementAffinity(DamageElementAffinity::Resistant),
        DamageSideEffectId::ElementResist,
        "damage_element_resist",
        40,
        2.0,
        0.08,
        -0.05,
    ),
    damage_side_effect(
        DamageCondition::ElementAffinity(DamageElementAffinity::Weak),
        DamageSideEffectId::ElementWeakness,
        "damage_element_weak",
        0,
        -2.0,
        0.16,
        0.12,
    ),
    damage_side_effect(
        DamageCondition::ElementAffinity(DamageElementAffinity::Absorbed),
        DamageSideEffectId::ElementAbsorb,
        "damage_element_absorb",
        140,
        8.0,
        0.2,
        -0.25,
    ),
];
const BASIC_STRIKE_DAMAGE_SIDE_EFFECTS: &[DamageSideEffectDef] = &[
    damage_side_effect(
        DamageCondition::WeakGuard,
        DamageSideEffectId::GuardPressure,
        "damage_guard_pressure",
        0,
        -3.0,
        0.08,
        0.04,
    ),
    damage_side_effect(
        DamageCondition::ElementAffinity(DamageElementAffinity::Resistant),
        DamageSideEffectId::ElementResist,
        "damage_strike_resist",
        40,
        1.5,
        0.08,
        -0.05,
    ),
    damage_side_effect(
        DamageCondition::ElementAffinity(DamageElementAffinity::Weak),
        DamageSideEffectId::ElementWeakness,
        "damage_strike_weak",
        0,
        -2.0,
        0.14,
        0.1,
    ),
    damage_side_effect(
        DamageCondition::ElementAffinity(DamageElementAffinity::Absorbed),
        DamageSideEffectId::ElementAbsorb,
        "damage_strike_absorb",
        120,
        6.0,
        0.18,
        -0.2,
    ),
];
const FOLLOWUP_DAMAGE_SIDE_EFFECTS: &[DamageSideEffectDef] = &[
    damage_side_effect(
        DamageCondition::WeakGuard,
        DamageSideEffectId::GuardPressure,
        "damage_followup_guard_pressure",
        0,
        -4.0,
        0.1,
        0.05,
    ),
    damage_side_effect(
        DamageCondition::CounterHit,
        DamageSideEffectId::CounterSurge,
        "damage_followup_counter",
        0,
        0.0,
        0.12,
        0.12,
    ),
    damage_side_effect(
        DamageCondition::Element(DamageElement::Strike),
        DamageSideEffectId::StatusExploit,
        "damage_followup_strike",
        0,
        0.0,
        0.06,
        0.04,
    ),
    damage_side_effect(
        DamageCondition::ElementAffinity(DamageElementAffinity::Resistant),
        DamageSideEffectId::ElementResist,
        "damage_followup_resist",
        40,
        2.0,
        0.08,
        -0.06,
    ),
    damage_side_effect(
        DamageCondition::ElementAffinity(DamageElementAffinity::Weak),
        DamageSideEffectId::ElementWeakness,
        "damage_followup_weak",
        0,
        -2.5,
        0.15,
        0.12,
    ),
    damage_side_effect(
        DamageCondition::ElementAffinity(DamageElementAffinity::Absorbed),
        DamageSideEffectId::ElementAbsorb,
        "damage_followup_absorb",
        140,
        7.0,
        0.2,
        -0.24,
    ),
];
const LAUNCHER_DAMAGE_SIDE_EFFECTS: &[DamageSideEffectDef] = &[
    damage_side_effect(
        DamageCondition::Airborne,
        DamageSideEffectId::JuggleBonus,
        "damage_launcher_juggle",
        0,
        0.0,
        0.12,
        0.12,
    ),
    damage_side_effect(
        DamageCondition::GuardBreak,
        DamageSideEffectId::GuardPressure,
        "damage_launcher_guard_crush",
        120,
        -8.0,
        0.18,
        0.15,
    ),
    damage_side_effect(
        DamageCondition::AttackerEquipment(EquipmentKind::HeavySeal),
        DamageSideEffectId::AccessorySurge,
        "damage_heavy_seal_launch",
        80,
        0.0,
        0.16,
        0.12,
    ),
    damage_side_effect(
        DamageCondition::Element(DamageElement::Blast),
        DamageSideEffectId::ElementBurst,
        "damage_blast_launch",
        140,
        0.0,
        0.2,
        0.16,
    ),
    damage_side_effect(
        DamageCondition::ElementAffinity(DamageElementAffinity::Resistant),
        DamageSideEffectId::ElementResist,
        "damage_launcher_resist",
        60,
        2.0,
        0.1,
        -0.08,
    ),
    damage_side_effect(
        DamageCondition::ElementAffinity(DamageElementAffinity::Weak),
        DamageSideEffectId::ElementWeakness,
        "damage_launcher_weak",
        0,
        -3.0,
        0.22,
        0.18,
    ),
    damage_side_effect(
        DamageCondition::ElementAffinity(DamageElementAffinity::Absorbed),
        DamageSideEffectId::ElementAbsorb,
        "damage_launcher_absorb",
        180,
        10.0,
        0.24,
        -0.3,
    ),
];
const GROUND_BOUNCE_DAMAGE_SIDE_EFFECTS: &[DamageSideEffectDef] = &[
    damage_side_effect(
        DamageCondition::Downed,
        DamageSideEffectId::DownedProration,
        "damage_downed_proration",
        0,
        0.0,
        0.06,
        -0.08,
    ),
    damage_side_effect(
        DamageCondition::LethalRaw,
        DamageSideEffectId::LethalPunctuation,
        "damage_bounce_finish",
        120,
        0.0,
        0.18,
        0.18,
    ),
    damage_side_effect(
        DamageCondition::Element(DamageElement::Earth),
        DamageSideEffectId::ElementBurst,
        "damage_earth_bounce",
        80,
        0.0,
        0.14,
        0.08,
    ),
    damage_side_effect(
        DamageCondition::ElementAffinity(DamageElementAffinity::Resistant),
        DamageSideEffectId::ElementResist,
        "damage_bounce_resist",
        60,
        2.0,
        0.1,
        -0.08,
    ),
    damage_side_effect(
        DamageCondition::ElementAffinity(DamageElementAffinity::Weak),
        DamageSideEffectId::ElementWeakness,
        "damage_bounce_weak",
        0,
        -3.0,
        0.2,
        0.16,
    ),
    damage_side_effect(
        DamageCondition::ElementAffinity(DamageElementAffinity::Absorbed),
        DamageSideEffectId::ElementAbsorb,
        "damage_bounce_absorb",
        180,
        10.0,
        0.24,
        -0.3,
    ),
];
const GRAB_DAMAGE_SIDE_EFFECTS: &[DamageSideEffectDef] = NO_DAMAGE_SIDE_EFFECTS;
const DASH_BODY_DAMAGE_SIDE_EFFECTS: &[DamageSideEffectDef] = &[
    damage_side_effect(
        DamageCondition::CounterHit,
        DamageSideEffectId::CounterSurge,
        "damage_dash_counter",
        0,
        0.0,
        0.12,
        0.12,
    ),
    damage_side_effect(
        DamageCondition::AttackerEquipment(EquipmentKind::DashCoil),
        DamageSideEffectId::AccessorySurge,
        "damage_dash_coil",
        0,
        0.0,
        0.16,
        0.1,
    ),
    damage_side_effect(
        DamageCondition::AttackerStyle(FighterStyleKind::Vector),
        DamageSideEffectId::StatusExploit,
        "damage_vector_dash",
        0,
        0.0,
        0.1,
        0.06,
    ),
    damage_side_effect(
        DamageCondition::ElementAffinity(DamageElementAffinity::Resistant),
        DamageSideEffectId::ElementResist,
        "damage_dash_resist",
        40,
        2.0,
        0.08,
        -0.06,
    ),
    damage_side_effect(
        DamageCondition::ElementAffinity(DamageElementAffinity::Weak),
        DamageSideEffectId::ElementWeakness,
        "damage_dash_weak",
        0,
        -2.0,
        0.16,
        0.14,
    ),
    damage_side_effect(
        DamageCondition::ElementAffinity(DamageElementAffinity::Absorbed),
        DamageSideEffectId::ElementAbsorb,
        "damage_dash_absorb",
        140,
        8.0,
        0.2,
        -0.24,
    ),
];
const AERIAL_SPIKE_DAMAGE_SIDE_EFFECTS: &[DamageSideEffectDef] = &[
    damage_side_effect(
        DamageCondition::Airborne,
        DamageSideEffectId::JuggleBonus,
        "damage_aerial_spike_clean",
        120,
        0.0,
        0.18,
        0.16,
    ),
    damage_side_effect(
        DamageCondition::LethalRaw,
        DamageSideEffectId::LethalPunctuation,
        "damage_spike_finish",
        160,
        0.0,
        0.22,
        0.22,
    ),
    damage_side_effect(
        DamageCondition::AttackerEquipment(EquipmentKind::AerialSpur),
        DamageSideEffectId::AccessorySurge,
        "damage_aerial_spur",
        120,
        0.0,
        0.18,
        0.12,
    ),
    damage_side_effect(
        DamageCondition::ElementAffinity(DamageElementAffinity::Resistant),
        DamageSideEffectId::ElementResist,
        "damage_spike_resist",
        60,
        2.0,
        0.1,
        -0.08,
    ),
    damage_side_effect(
        DamageCondition::ElementAffinity(DamageElementAffinity::Weak),
        DamageSideEffectId::ElementWeakness,
        "damage_spike_weak",
        0,
        -3.0,
        0.2,
        0.16,
    ),
    damage_side_effect(
        DamageCondition::ElementAffinity(DamageElementAffinity::Absorbed),
        DamageSideEffectId::ElementAbsorb,
        "damage_spike_absorb",
        180,
        10.0,
        0.24,
        -0.3,
    ),
];
const COUNTER_BLOW_DAMAGE_SIDE_EFFECTS: &[DamageSideEffectDef] = &[
    damage_side_effect(
        DamageCondition::GuardBreak,
        DamageSideEffectId::GuardPressure,
        "damage_counter_guard_crush",
        160,
        -10.0,
        0.22,
        0.2,
    ),
    damage_side_effect(
        DamageCondition::LowHealth,
        DamageSideEffectId::CounterSurge,
        "damage_counter_low_health",
        0,
        0.0,
        0.16,
        0.12,
    ),
    damage_side_effect(
        DamageCondition::AttackerEquipment(EquipmentKind::CounterCell),
        DamageSideEffectId::AccessorySurge,
        "damage_counter_cell",
        120,
        0.0,
        0.18,
        0.14,
    ),
    damage_side_effect(
        DamageCondition::ElementAffinity(DamageElementAffinity::Resistant),
        DamageSideEffectId::ElementResist,
        "damage_counter_resist",
        60,
        3.0,
        0.1,
        -0.08,
    ),
    damage_side_effect(
        DamageCondition::ElementAffinity(DamageElementAffinity::Weak),
        DamageSideEffectId::ElementWeakness,
        "damage_counter_weak",
        0,
        -3.0,
        0.22,
        0.18,
    ),
    damage_side_effect(
        DamageCondition::ElementAffinity(DamageElementAffinity::Absorbed),
        DamageSideEffectId::ElementAbsorb,
        "damage_counter_absorb",
        180,
        12.0,
        0.24,
        -0.32,
    ),
];
const ITEM_HEAVY_DAMAGE_SIDE_EFFECTS: &[DamageSideEffectDef] = &[
    damage_side_effect(
        DamageCondition::ItemSource,
        DamageSideEffectId::ItemCrush,
        "damage_item_crush",
        120,
        -4.0,
        0.14,
        0.1,
    ),
    damage_side_effect(
        DamageCondition::LethalRaw,
        DamageSideEffectId::LethalPunctuation,
        "damage_item_finish",
        180,
        0.0,
        0.22,
        0.18,
    ),
    damage_side_effect(
        DamageCondition::ElementAffinity(DamageElementAffinity::Resistant),
        DamageSideEffectId::ElementResist,
        "damage_item_resist",
        60,
        2.0,
        0.1,
        -0.08,
    ),
    damage_side_effect(
        DamageCondition::ElementAffinity(DamageElementAffinity::Weak),
        DamageSideEffectId::ElementWeakness,
        "damage_item_weak",
        0,
        -3.0,
        0.2,
        0.16,
    ),
    damage_side_effect(
        DamageCondition::ElementAffinity(DamageElementAffinity::Absorbed),
        DamageSideEffectId::ElementAbsorb,
        "damage_item_absorb",
        180,
        10.0,
        0.24,
        -0.3,
    ),
];

const LIGHT1_EVENTS: [MoveTimelineEvent; 7] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_A_s"),
    feedback_event(390, FeedbackPhase::PreHit, "trail_A_s"),
    timeline_event(
        430,
        MoveTimelineEventKind::Motion {
            forward: 0.22,
            lift: 0.0,
        },
    ),
    attack_event(440, AttackPayloadId::AsBeat1),
    feedback_event(686, FeedbackPhase::Aftermath, "recover_anim_A_s"),
    timeline_event(720, MoveTimelineEventKind::NextTech),
    timeline_event(800, MoveTimelineEventKind::Recover),
];

const LIGHT2_EVENTS: [MoveTimelineEvent; 7] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_A_ss"),
    feedback_event(410, FeedbackPhase::PreHit, "trail_A_ss"),
    timeline_event(
        430,
        MoveTimelineEventKind::Motion {
            forward: 0.18,
            lift: 0.0,
        },
    ),
    attack_event(440, AttackPayloadId::AssBeat1),
    feedback_event(650, FeedbackPhase::Aftermath, "recover_anim_A_ss"),
    timeline_event(690, MoveTimelineEventKind::NextTech),
    timeline_event(850, MoveTimelineEventKind::Recover),
];

const DOG_LIGHT1_EVENTS: [MoveTimelineEvent; 7] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_dog_bite_1"),
    feedback_event(260, FeedbackPhase::PreHit, "trail_dog_bite_1"),
    timeline_event(
        280,
        MoveTimelineEventKind::Motion {
            forward: 0.34,
            lift: 0.0,
        },
    ),
    attack_event(300, AttackPayloadId::DogBite1),
    feedback_event(500, FeedbackPhase::Aftermath, "recover_dog_bite_1"),
    timeline_event(530, MoveTimelineEventKind::NextTech),
    timeline_event(620, MoveTimelineEventKind::Recover),
];

const DOG_LIGHT2_EVENTS: [MoveTimelineEvent; 7] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_dog_bite_2"),
    feedback_event(280, FeedbackPhase::PreHit, "trail_dog_bite_2"),
    timeline_event(
        305,
        MoveTimelineEventKind::Motion {
            forward: 0.38,
            lift: 0.0,
        },
    ),
    attack_event(325, AttackPayloadId::DogBite2),
    feedback_event(530, FeedbackPhase::Aftermath, "recover_dog_bite_2"),
    timeline_event(560, MoveTimelineEventKind::NextTech),
    timeline_event(680, MoveTimelineEventKind::Recover),
];

const FOX_LIGHT1_EVENTS: [MoveTimelineEvent; 7] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_fox_swipe_1"),
    feedback_event(170, FeedbackPhase::PreHit, "trail_fox_swipe_1"),
    timeline_event(
        185,
        MoveTimelineEventKind::Motion {
            forward: 0.16,
            lift: 0.0,
        },
    ),
    attack_event(200, AttackPayloadId::FoxSwipe1),
    feedback_event(360, FeedbackPhase::Aftermath, "recover_fox_swipe_1"),
    timeline_event(390, MoveTimelineEventKind::NextTech),
    timeline_event(470, MoveTimelineEventKind::Recover),
];

const FOX_LIGHT2_EVENTS: [MoveTimelineEvent; 7] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_fox_swipe_2"),
    feedback_event(160, FeedbackPhase::PreHit, "trail_fox_swipe_2"),
    timeline_event(
        175,
        MoveTimelineEventKind::Motion {
            forward: 0.18,
            lift: 0.0,
        },
    ),
    attack_event(190, AttackPayloadId::FoxSwipe2),
    feedback_event(340, FeedbackPhase::Aftermath, "recover_fox_swipe_2"),
    timeline_event(370, MoveTimelineEventKind::NextTech),
    timeline_event(460, MoveTimelineEventKind::Recover),
];

const PANDA_LIGHT1_EVENTS: [MoveTimelineEvent; 7] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_panda_palm_1"),
    feedback_event(360, FeedbackPhase::PreHit, "trail_panda_palm_1"),
    timeline_event(
        380,
        MoveTimelineEventKind::Motion {
            forward: 0.24,
            lift: 0.0,
        },
    ),
    attack_event(410, AttackPayloadId::PandaPalm1),
    feedback_event(680, FeedbackPhase::Aftermath, "recover_panda_palm_1"),
    timeline_event(720, MoveTimelineEventKind::NextTech),
    timeline_event(860, MoveTimelineEventKind::Recover),
];

const PANDA_LIGHT2_EVENTS: [MoveTimelineEvent; 7] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_panda_palm_2"),
    feedback_event(390, FeedbackPhase::PreHit, "trail_panda_palm_2"),
    timeline_event(
        415,
        MoveTimelineEventKind::Motion {
            forward: 0.28,
            lift: 0.0,
        },
    ),
    attack_event(445, AttackPayloadId::PandaPalm2),
    feedback_event(720, FeedbackPhase::Aftermath, "recover_panda_palm_2"),
    timeline_event(760, MoveTimelineEventKind::NextTech),
    timeline_event(920, MoveTimelineEventKind::Recover),
];

const PIG_LIGHT1_EVENTS: [MoveTimelineEvent; 7] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_pig_cat_light_1"),
    feedback_event(390, FeedbackPhase::PreHit, "trail_pig_cat_light_1"),
    timeline_event(
        430,
        MoveTimelineEventKind::Motion {
            forward: 0.22,
            lift: 0.0,
        },
    ),
    attack_event(440, AttackPayloadId::AsBeat1),
    feedback_event(686, FeedbackPhase::Aftermath, "recover_pig_cat_light_1"),
    timeline_event(720, MoveTimelineEventKind::NextTech),
    timeline_event(800, MoveTimelineEventKind::Recover),
];

const PIG_LIGHT2_EVENTS: [MoveTimelineEvent; 7] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_pig_cat_light_2"),
    feedback_event(410, FeedbackPhase::PreHit, "trail_pig_cat_light_2"),
    timeline_event(
        430,
        MoveTimelineEventKind::Motion {
            forward: 0.18,
            lift: 0.0,
        },
    ),
    attack_event(440, AttackPayloadId::AssBeat1),
    feedback_event(650, FeedbackPhase::Aftermath, "recover_pig_cat_light_2"),
    timeline_event(690, MoveTimelineEventKind::NextTech),
    timeline_event(850, MoveTimelineEventKind::Recover),
];

const HEAVY_STEP_EVENTS: [MoveTimelineEvent; 7] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_heavy_step"),
    timeline_event(
        80,
        MoveTimelineEventKind::Motion {
            forward: 5.4,
            lift: 0.0,
        },
    ),
    feedback_event(120, FeedbackPhase::PreHit, "trail_heavy_step"),
    attack_event(180, AttackPayloadId::HeavyStep),
    feedback_event(300, FeedbackPhase::Aftermath, "recover_heavy_step"),
    timeline_event(330, MoveTimelineEventKind::NextTech),
    timeline_event(500, MoveTimelineEventKind::Recover),
];

const KIRIAGE_EVENTS: [MoveTimelineEvent; 10] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_kiriage"),
    timeline_event(
        90,
        MoveTimelineEventKind::Motion {
            forward: 1.4,
            lift: 0.0,
        },
    ),
    feedback_event(170, FeedbackPhase::PreHit, "trail_kiriage"),
    timeline_event(
        220,
        MoveTimelineEventKind::Motion {
            forward: 0.8,
            lift: 3.2,
        },
    ),
    attack_event(360, AttackPayloadId::KiriageBeat2),
    timeline_event(
        380,
        MoveTimelineEventKind::Motion {
            forward: 0.4,
            lift: 0.0,
        },
    ),
    timeline_event(430, MoveTimelineEventKind::Stop),
    feedback_event(500, FeedbackPhase::Aftermath, "post_hit_kiriage"),
    timeline_event(650, MoveTimelineEventKind::NextTech),
    timeline_event(960, MoveTimelineEventKind::Recover),
];

const DOG_HEAVY_EVENTS: [MoveTimelineEvent; 7] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_dog_shoulder"),
    timeline_event(
        90,
        MoveTimelineEventKind::Motion {
            forward: 4.8,
            lift: 0.0,
        },
    ),
    feedback_event(150, FeedbackPhase::PreHit, "trail_dog_shoulder"),
    attack_event(220, AttackPayloadId::DogShoulderStep),
    feedback_event(420, FeedbackPhase::Aftermath, "recover_dog_shoulder"),
    timeline_event(455, MoveTimelineEventKind::NextTech),
    timeline_event(620, MoveTimelineEventKind::Recover),
];

const DOG_HEAVY2_EVENTS: [MoveTimelineEvent; 10] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_dog_launch_bite"),
    timeline_event(
        140,
        MoveTimelineEventKind::Motion {
            forward: 1.0,
            lift: 0.0,
        },
    ),
    feedback_event(240, FeedbackPhase::PreHit, "trail_dog_launch_bite"),
    timeline_event(
        290,
        MoveTimelineEventKind::Motion {
            forward: 0.9,
            lift: 2.4,
        },
    ),
    attack_event(380, AttackPayloadId::DogLaunchBite),
    timeline_event(
        420,
        MoveTimelineEventKind::Motion {
            forward: 0.7,
            lift: 0.0,
        },
    ),
    timeline_event(520, MoveTimelineEventKind::Stop),
    feedback_event(590, FeedbackPhase::Aftermath, "recover_dog_launch_bite"),
    timeline_event(720, MoveTimelineEventKind::NextTech),
    timeline_event(1050, MoveTimelineEventKind::Recover),
];

const FOX_HEAVY_EVENTS: [MoveTimelineEvent; 7] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_fox_skitter"),
    timeline_event(
        40,
        MoveTimelineEventKind::Motion {
            forward: 6.2,
            lift: 0.0,
        },
    ),
    feedback_event(80, FeedbackPhase::PreHit, "trail_fox_skitter"),
    attack_event(125, AttackPayloadId::FoxSkitterStep),
    feedback_event(230, FeedbackPhase::Aftermath, "recover_fox_skitter"),
    timeline_event(250, MoveTimelineEventKind::NextTech),
    timeline_event(360, MoveTimelineEventKind::Recover),
];

const FOX_HEAVY2_EVENTS: [MoveTimelineEvent; 10] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_fox_flip"),
    timeline_event(
        70,
        MoveTimelineEventKind::Motion {
            forward: 1.8,
            lift: 0.0,
        },
    ),
    feedback_event(120, FeedbackPhase::PreHit, "trail_fox_flip"),
    timeline_event(
        160,
        MoveTimelineEventKind::Motion {
            forward: 0.8,
            lift: 2.8,
        },
    ),
    attack_event(215, AttackPayloadId::FoxFlipLaunch),
    timeline_event(
        260,
        MoveTimelineEventKind::Motion {
            forward: 0.4,
            lift: 0.0,
        },
    ),
    timeline_event(330, MoveTimelineEventKind::Stop),
    feedback_event(390, FeedbackPhase::Aftermath, "recover_fox_flip"),
    timeline_event(500, MoveTimelineEventKind::NextTech),
    timeline_event(760, MoveTimelineEventKind::Recover),
];

const BEE_LIGHT1_EVENTS: [MoveTimelineEvent; 5] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_bee_worker_swarm"),
    bee_skill_event(0, BeeSkillId::WorkerSwarm),
    feedback_event(45, FeedbackPhase::PreHit, "trail_bee_worker_swarm"),
    feedback_event(220, FeedbackPhase::Aftermath, "recover_bee_worker_swarm"),
    timeline_event(360, MoveTimelineEventKind::Recover),
];

const BEE_LIGHT2_EVENTS: [MoveTimelineEvent; 7] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_bee_cross_sting"),
    feedback_event(150, FeedbackPhase::PreHit, "trail_bee_cross_sting"),
    timeline_event(
        170,
        MoveTimelineEventKind::Motion {
            forward: 0.28,
            lift: 0.0,
        },
    ),
    attack_event(190, AttackPayloadId::BeeCrossSting),
    feedback_event(350, FeedbackPhase::Aftermath, "recover_bee_cross_sting"),
    timeline_event(380, MoveTimelineEventKind::NextTech),
    timeline_event(470, MoveTimelineEventKind::Recover),
];

const BEE_HEAVY_EVENTS: [MoveTimelineEvent; 7] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_bee_piercing_step"),
    timeline_event(
        45,
        MoveTimelineEventKind::Motion {
            forward: 6.0,
            lift: 0.0,
        },
    ),
    feedback_event(90, FeedbackPhase::PreHit, "trail_bee_piercing_step"),
    attack_event(135, AttackPayloadId::BeePiercingStep),
    feedback_event(260, FeedbackPhase::Aftermath, "recover_bee_piercing_step"),
    timeline_event(290, MoveTimelineEventKind::NextTech),
    timeline_event(410, MoveTimelineEventKind::Recover),
];

const BEE_HEAVY2_EVENTS: [MoveTimelineEvent; 7] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_bee_homing_sting"),
    timeline_event(
        55,
        MoveTimelineEventKind::Motion {
            forward: 0.35,
            lift: 0.0,
        },
    ),
    feedback_event(85, FeedbackPhase::PreHit, "trail_bee_homing_sting"),
    bee_skill_event(120, BeeSkillId::HomingSting),
    timeline_event(210, MoveTimelineEventKind::Stop),
    feedback_event(320, FeedbackPhase::Aftermath, "recover_bee_homing_sting"),
    timeline_event(560, MoveTimelineEventKind::Recover),
];

const PENGUIN_LIGHT1_EVENTS: [MoveTimelineEvent; 6] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_special_cast"),
    feedback_event(90, FeedbackPhase::PreHit, "release_special_cast"),
    penguin_skill_event(115, PenguinSkillId::SnowflakeShot),
    feedback_event(140, FeedbackPhase::Aftermath, "recover_special_cast"),
    timeline_event(150, MoveTimelineEventKind::NextTech),
    timeline_event(160, MoveTimelineEventKind::Recover),
];

const PENGUIN_LIGHT2_EVENTS: [MoveTimelineEvent; 6] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_special_cast"),
    feedback_event(105, FeedbackPhase::PreHit, "release_special_cast"),
    penguin_skill_event(130, PenguinSkillId::SnowflakeShot),
    feedback_event(155, FeedbackPhase::Aftermath, "recover_special_cast"),
    timeline_event(165, MoveTimelineEventKind::NextTech),
    timeline_event(175, MoveTimelineEventKind::Recover),
];

const PENGUIN_HEAVY_EVENTS: [MoveTimelineEvent; 6] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_special_cast"),
    penguin_skill_event(90, PenguinSkillId::SnowmanDrop),
    feedback_event(210, FeedbackPhase::PreHit, "release_special_cast"),
    feedback_event(500, FeedbackPhase::Aftermath, "recover_special_cast"),
    timeline_event(540, MoveTimelineEventKind::NextTech),
    timeline_event(760, MoveTimelineEventKind::Recover),
];

const PENGUIN_HEAVY2_EVENTS: [MoveTimelineEvent; 11] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_penguin_sled_scoop"),
    timeline_event(
        150,
        MoveTimelineEventKind::Motion {
            forward: 1.1,
            lift: 0.0,
        },
    ),
    feedback_event(300, FeedbackPhase::PreHit, "trail_penguin_sled_scoop"),
    penguin_skill_event(330, PenguinSkillId::SnowfortCannon),
    timeline_event(
        360,
        MoveTimelineEventKind::Motion {
            forward: 0.6,
            lift: 2.6,
        },
    ),
    attack_event(450, AttackPayloadId::PenguinSledScoop),
    timeline_event(
        500,
        MoveTimelineEventKind::Motion {
            forward: 0.28,
            lift: 0.0,
        },
    ),
    timeline_event(610, MoveTimelineEventKind::Stop),
    feedback_event(680, FeedbackPhase::Aftermath, "recover_penguin_sled_scoop"),
    timeline_event(820, MoveTimelineEventKind::NextTech),
    timeline_event(1120, MoveTimelineEventKind::Recover),
];

const PANDA_HEAVY_EVENTS: [MoveTimelineEvent; 7] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_panda_weight"),
    timeline_event(
        150,
        MoveTimelineEventKind::Motion {
            forward: 3.8,
            lift: 0.0,
        },
    ),
    feedback_event(250, FeedbackPhase::PreHit, "trail_panda_weight"),
    attack_event(340, AttackPayloadId::PandaWeightShift),
    feedback_event(520, FeedbackPhase::Aftermath, "recover_panda_weight"),
    timeline_event(560, MoveTimelineEventKind::NextTech),
    timeline_event(780, MoveTimelineEventKind::Recover),
];

const PANDA_HEAVY2_EVENTS: [MoveTimelineEvent; 10] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_panda_scoop"),
    timeline_event(
        160,
        MoveTimelineEventKind::Motion {
            forward: 0.9,
            lift: 0.0,
        },
    ),
    feedback_event(300, FeedbackPhase::PreHit, "trail_panda_scoop"),
    timeline_event(
        380,
        MoveTimelineEventKind::Motion {
            forward: 0.7,
            lift: 3.6,
        },
    ),
    attack_event(500, AttackPayloadId::PandaRisingScoop),
    timeline_event(
        550,
        MoveTimelineEventKind::Motion {
            forward: 0.3,
            lift: 0.0,
        },
    ),
    timeline_event(650, MoveTimelineEventKind::Stop),
    feedback_event(720, FeedbackPhase::Aftermath, "recover_panda_scoop"),
    timeline_event(860, MoveTimelineEventKind::NextTech),
    timeline_event(1180, MoveTimelineEventKind::Recover),
];

const PIG_HEAVY_EVENTS: [MoveTimelineEvent; 7] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_pig_ham_swing"),
    feedback_event(280, FeedbackPhase::PreHit, "charge_pig_ham_swing"),
    timeline_event(
        330,
        MoveTimelineEventKind::Motion {
            forward: 0.32,
            lift: 0.0,
        },
    ),
    charged_attack_event(
        PIG_HEAVY_ATTACK_MS,
        AttackPayloadId::PigHamSwingTap,
        AttackPayloadId::PigHamSwingPartial,
        AttackPayloadId::PigHamSwingFull,
    ),
    timeline_event(980, MoveTimelineEventKind::Stop),
    feedback_event(1040, FeedbackPhase::Aftermath, "recover_pig_ham_swing"),
    timeline_event(1280, MoveTimelineEventKind::Recover),
];

const PIG_HEAVY2_EVENTS: [MoveTimelineEvent; 10] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_pig_ham_launcher"),
    timeline_event(
        210,
        MoveTimelineEventKind::Motion {
            forward: 0.65,
            lift: 0.0,
        },
    ),
    feedback_event(420, FeedbackPhase::PreHit, "trail_pig_ham_launcher"),
    timeline_event(
        520,
        MoveTimelineEventKind::Motion {
            forward: 0.55,
            lift: 3.8,
        },
    ),
    attack_event(660, AttackPayloadId::PigHamLauncher),
    timeline_event(
        720,
        MoveTimelineEventKind::Motion {
            forward: 0.2,
            lift: 0.0,
        },
    ),
    timeline_event(820, MoveTimelineEventKind::Stop),
    feedback_event(900, FeedbackPhase::Aftermath, "recover_pig_ham_launcher"),
    timeline_event(1060, MoveTimelineEventKind::NextTech),
    timeline_event(1380, MoveTimelineEventKind::Recover),
];

const ULTIMATE_STARTUP_EVENTS: [MoveTimelineEvent; 9] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_ultimate_beast"),
    feedback_event(70, FeedbackPhase::PreHit, "trail_ultimate_catch"),
    timeline_event(
        80,
        MoveTimelineEventKind::Motion {
            forward: 14.0,
            lift: 0.0,
        },
    ),
    attack_event(80, AttackPayloadId::UltimateCatch),
    timeline_event(
        180,
        MoveTimelineEventKind::Motion {
            forward: 10.0,
            lift: 0.0,
        },
    ),
    timeline_event(
        300,
        MoveTimelineEventKind::Motion {
            forward: 8.0,
            lift: 0.0,
        },
    ),
    timeline_event(430, MoveTimelineEventKind::Stop),
    feedback_event(520, FeedbackPhase::Aftermath, "recover_ultimate_whiff"),
    timeline_event(780, MoveTimelineEventKind::Recover),
];

const ULTIMATE_RUSH_EVENTS: [MoveTimelineEvent; 15] = [
    feedback_event(0, FeedbackPhase::Startup, "ultimate_lock_start"),
    timeline_event(
        30,
        MoveTimelineEventKind::Motion {
            forward: 0.55,
            lift: 0.0,
        },
    ),
    feedback_event(70, FeedbackPhase::PreHit, "trail_ultimate_scratch"),
    attack_event(90, AttackPayloadId::UltimateScratchLight),
    feedback_event(220, FeedbackPhase::PreHit, "trail_ultimate_scratch"),
    attack_event(240, AttackPayloadId::UltimateScratchLight),
    timeline_event(
        280,
        MoveTimelineEventKind::Motion {
            forward: 0.4,
            lift: 0.0,
        },
    ),
    feedback_event(370, FeedbackPhase::PreHit, "trail_ultimate_scratch"),
    attack_event(390, AttackPayloadId::UltimateScratchHeavy),
    feedback_event(520, FeedbackPhase::PreHit, "trail_ultimate_scratch"),
    attack_event(540, AttackPayloadId::UltimateScratchHeavy),
    feedback_event(690, FeedbackPhase::PreHit, "charge_ultimate_bomb"),
    attack_event(820, AttackPayloadId::UltimateBomb),
    feedback_event(960, FeedbackPhase::Aftermath, "recover_ultimate_bomb"),
    timeline_event(1260, MoveTimelineEventKind::Recover),
];

const DOG_ULTIMATE_STARTUP_EVENTS: [MoveTimelineEvent; 6] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_dog_ultimate_beast"),
    feedback_event(210, FeedbackPhase::PreHit, "trail_dog_ultimate_catch"),
    timeline_event(
        270,
        MoveTimelineEventKind::Motion {
            forward: 1.6,
            lift: 0.0,
        },
    ),
    attack_event(360, AttackPayloadId::DogUltimateCatch),
    feedback_event(530, FeedbackPhase::Aftermath, "recover_dog_ultimate_whiff"),
    timeline_event(720, MoveTimelineEventKind::Recover),
];

const DOG_ULTIMATE_RUSH_EVENTS: [MoveTimelineEvent; 15] = [
    feedback_event(0, FeedbackPhase::Startup, "dog_ultimate_lock_start"),
    timeline_event(
        40,
        MoveTimelineEventKind::Motion {
            forward: 0.65,
            lift: 0.0,
        },
    ),
    feedback_event(80, FeedbackPhase::PreHit, "trail_dog_ultimate_scratch"),
    attack_event(105, AttackPayloadId::DogUltimateScratchLight),
    feedback_event(245, FeedbackPhase::PreHit, "trail_dog_ultimate_scratch"),
    attack_event(275, AttackPayloadId::DogUltimateScratchLight),
    timeline_event(
        330,
        MoveTimelineEventKind::Motion {
            forward: 0.45,
            lift: 0.0,
        },
    ),
    feedback_event(430, FeedbackPhase::PreHit, "trail_dog_ultimate_scratch"),
    attack_event(465, AttackPayloadId::DogUltimateScratchHeavy),
    feedback_event(590, FeedbackPhase::PreHit, "trail_dog_ultimate_scratch"),
    attack_event(625, AttackPayloadId::DogUltimateScratchHeavy),
    feedback_event(800, FeedbackPhase::PreHit, "charge_dog_ultimate_bomb"),
    attack_event(940, AttackPayloadId::DogUltimateBomb),
    feedback_event(1100, FeedbackPhase::Aftermath, "recover_dog_ultimate_bomb"),
    timeline_event(1390, MoveTimelineEventKind::Recover),
];

const FOX_ULTIMATE_STARTUP_EVENTS: [MoveTimelineEvent; 6] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_fox_ultimate_beast"),
    feedback_event(150, FeedbackPhase::PreHit, "trail_fox_ultimate_catch"),
    timeline_event(
        190,
        MoveTimelineEventKind::Motion {
            forward: 1.9,
            lift: 0.0,
        },
    ),
    attack_event(260, AttackPayloadId::FoxUltimateCatch),
    feedback_event(390, FeedbackPhase::Aftermath, "recover_fox_ultimate_whiff"),
    timeline_event(560, MoveTimelineEventKind::Recover),
];

const FOX_ULTIMATE_RUSH_EVENTS: [MoveTimelineEvent; 15] = [
    feedback_event(0, FeedbackPhase::Startup, "fox_ultimate_lock_start"),
    timeline_event(
        25,
        MoveTimelineEventKind::Motion {
            forward: 0.72,
            lift: 0.0,
        },
    ),
    feedback_event(50, FeedbackPhase::PreHit, "trail_fox_ultimate_scratch"),
    attack_event(70, AttackPayloadId::FoxUltimateScratchLight),
    feedback_event(165, FeedbackPhase::PreHit, "trail_fox_ultimate_scratch"),
    attack_event(190, AttackPayloadId::FoxUltimateScratchLight),
    timeline_event(
        230,
        MoveTimelineEventKind::Motion {
            forward: 0.56,
            lift: 0.0,
        },
    ),
    feedback_event(290, FeedbackPhase::PreHit, "trail_fox_ultimate_scratch"),
    attack_event(315, AttackPayloadId::FoxUltimateScratchHeavy),
    feedback_event(405, FeedbackPhase::PreHit, "trail_fox_ultimate_scratch"),
    attack_event(430, AttackPayloadId::FoxUltimateScratchHeavy),
    feedback_event(540, FeedbackPhase::PreHit, "charge_fox_ultimate_bomb"),
    attack_event(650, AttackPayloadId::FoxUltimateBomb),
    feedback_event(790, FeedbackPhase::Aftermath, "recover_fox_ultimate_bomb"),
    timeline_event(1040, MoveTimelineEventKind::Recover),
];

const BEE_ULTIMATE_STARTUP_EVENTS: [MoveTimelineEvent; 4] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_bee_ultimate_swarm"),
    bee_skill_event(120, BeeSkillId::UltimateSwarm),
    feedback_event(360, FeedbackPhase::Aftermath, "recover_bee_ultimate_swarm"),
    timeline_event(620, MoveTimelineEventKind::Recover),
];

const BEE_LEGACY_ULTIMATE_STARTUP_EVENTS: [MoveTimelineEvent; 12] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_bee_ultimate_swarm"),
    timeline_event(
        80,
        MoveTimelineEventKind::Motion {
            forward: 0.35,
            lift: 0.0,
        },
    ),
    bee_skill_event(110, BeeSkillId::WorkerSwarm),
    feedback_event(180, FeedbackPhase::PreHit, "trail_bee_ultimate_scratch"),
    bee_skill_event(220, BeeSkillId::HomingSting),
    bee_skill_event(360, BeeSkillId::WorkerSwarm),
    feedback_event(460, FeedbackPhase::PreHit, "charge_bee_ultimate_bomb"),
    bee_skill_event(500, BeeSkillId::HoneyGlob),
    bee_skill_event(620, BeeSkillId::HomingSting),
    attack_event(720, AttackPayloadId::BeeUltimateBomb),
    feedback_event(860, FeedbackPhase::Aftermath, "recover_bee_ultimate_bomb"),
    timeline_event(1040, MoveTimelineEventKind::Recover),
];

const BEE_LEGACY_ULTIMATE_RUSH_EVENTS: [MoveTimelineEvent; 15] = [
    feedback_event(0, FeedbackPhase::Startup, "bee_ultimate_lock_start"),
    timeline_event(
        30,
        MoveTimelineEventKind::Motion {
            forward: 0.68,
            lift: 0.0,
        },
    ),
    feedback_event(70, FeedbackPhase::PreHit, "trail_bee_ultimate_scratch"),
    attack_event(95, AttackPayloadId::BeeUltimateScratchLight),
    feedback_event(210, FeedbackPhase::PreHit, "trail_bee_ultimate_scratch"),
    attack_event(235, AttackPayloadId::BeeUltimateScratchLight),
    timeline_event(
        280,
        MoveTimelineEventKind::Motion {
            forward: 0.5,
            lift: 0.0,
        },
    ),
    feedback_event(350, FeedbackPhase::PreHit, "trail_bee_ultimate_scratch"),
    attack_event(380, AttackPayloadId::BeeUltimateScratchHeavy),
    feedback_event(490, FeedbackPhase::PreHit, "trail_bee_ultimate_scratch"),
    attack_event(520, AttackPayloadId::BeeUltimateScratchHeavy),
    feedback_event(650, FeedbackPhase::PreHit, "charge_bee_ultimate_bomb"),
    attack_event(780, AttackPayloadId::BeeUltimateBomb),
    feedback_event(930, FeedbackPhase::Aftermath, "recover_bee_ultimate_bomb"),
    timeline_event(1180, MoveTimelineEventKind::Recover),
];

const PENGUIN_ULTIMATE_ICE_FIELD_EVENTS: [MoveTimelineEvent; 4] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_penguin_ice_field"),
    penguin_skill_event(0, PenguinSkillId::UltimateIceField),
    feedback_event(360, FeedbackPhase::Aftermath, "recover_penguin_ice_field"),
    timeline_event(620, MoveTimelineEventKind::Recover),
];

const PENGUIN_ULTIMATE_SLOPE_EVENTS: [MoveTimelineEvent; 6] = [
    penguin_skill_event(0, PenguinSkillId::SnowSlopeRide),
    timeline_event(
        0,
        MoveTimelineEventKind::Motion {
            forward: PENGUIN_SLOPE_LAUNCH_FORWARD,
            lift: 0.0,
        },
    ),
    attack_event(0, AttackPayloadId::PenguinUltimateSlopeCrash),
    timeline_event(
        130,
        MoveTimelineEventKind::Motion {
            forward: PENGUIN_SLOPE_EXIT_FORWARD,
            lift: 0.0,
        },
    ),
    timeline_event(540, MoveTimelineEventKind::Stop),
    timeline_event(680, MoveTimelineEventKind::Recover),
];

const CHICK_ULTIMATE_STARTUP_EVENTS: [MoveTimelineEvent; 5] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_chick_egg_burst"),
    feedback_event(80, FeedbackPhase::PreHit, "release_chick_egg_burst"),
    chick_skill_event(100, ChickSkillId::UltimateEggBurst),
    feedback_event(360, FeedbackPhase::Aftermath, "recover_chick_egg_burst"),
    timeline_event(560, MoveTimelineEventKind::Recover),
];

const PANDA_ULTIMATE_STARTUP_EVENTS: [MoveTimelineEvent; 6] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_panda_ultimate_beast"),
    feedback_event(260, FeedbackPhase::PreHit, "trail_panda_ultimate_catch"),
    timeline_event(
        340,
        MoveTimelineEventKind::Motion {
            forward: 1.2,
            lift: 0.0,
        },
    ),
    attack_event(430, AttackPayloadId::PandaUltimateCatch),
    feedback_event(
        640,
        FeedbackPhase::Aftermath,
        "recover_panda_ultimate_whiff",
    ),
    timeline_event(840, MoveTimelineEventKind::Recover),
];

const PANDA_ULTIMATE_RUSH_EVENTS: [MoveTimelineEvent; 15] = [
    feedback_event(0, FeedbackPhase::Startup, "panda_ultimate_lock_start"),
    timeline_event(
        55,
        MoveTimelineEventKind::Motion {
            forward: 0.42,
            lift: 0.0,
        },
    ),
    feedback_event(95, FeedbackPhase::PreHit, "trail_panda_ultimate_scratch"),
    attack_event(125, AttackPayloadId::PandaUltimateScratchLight),
    feedback_event(285, FeedbackPhase::PreHit, "trail_panda_ultimate_scratch"),
    attack_event(320, AttackPayloadId::PandaUltimateScratchLight),
    timeline_event(
        390,
        MoveTimelineEventKind::Motion {
            forward: 0.28,
            lift: 0.0,
        },
    ),
    feedback_event(500, FeedbackPhase::PreHit, "trail_panda_ultimate_scratch"),
    attack_event(545, AttackPayloadId::PandaUltimateScratchHeavy),
    feedback_event(700, FeedbackPhase::PreHit, "trail_panda_ultimate_scratch"),
    attack_event(745, AttackPayloadId::PandaUltimateScratchHeavy),
    feedback_event(930, FeedbackPhase::PreHit, "charge_panda_ultimate_bomb"),
    attack_event(1080, AttackPayloadId::PandaUltimateBomb),
    feedback_event(
        1240,
        FeedbackPhase::Aftermath,
        "recover_panda_ultimate_bomb",
    ),
    timeline_event(1560, MoveTimelineEventKind::Recover),
];

const PIG_ULTIMATE_STARTUP_EVENTS: [MoveTimelineEvent; 6] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_pig_unblockable_grab"),
    feedback_event(760, FeedbackPhase::PreHit, "trail_pig_unblockable_grab"),
    timeline_event(
        880,
        MoveTimelineEventKind::Motion {
            forward: 0.55,
            lift: 0.0,
        },
    ),
    attack_event(980, AttackPayloadId::PigUltimateCatch),
    feedback_event(1600, FeedbackPhase::Aftermath, "recover_pig_ultimate_whiff"),
    timeline_event(2200, MoveTimelineEventKind::Recover),
];

const PIG_ULTIMATE_RUSH_EVENTS: [MoveTimelineEvent; 12] = [
    feedback_event(0, FeedbackPhase::Startup, "pig_grab_lock_start"),
    timeline_event(
        210,
        MoveTimelineEventKind::Motion {
            forward: 0.1,
            lift: 0.0,
        },
    ),
    feedback_event(220, FeedbackPhase::PreHit, "pig_grab_crush"),
    attack_event(240, AttackPayloadId::PigUltimateScratchLight),
    timeline_event(
        590,
        MoveTimelineEventKind::Motion {
            forward: 0.12,
            lift: 0.0,
        },
    ),
    feedback_event(600, FeedbackPhase::PreHit, "pig_grab_meat_slam"),
    attack_event(640, AttackPayloadId::PigUltimateScratchHeavy),
    timeline_event(
        1110,
        MoveTimelineEventKind::Motion {
            forward: 0.18,
            lift: 0.0,
        },
    ),
    feedback_event(1120, FeedbackPhase::PreHit, "charge_pig_ultimate_bomb"),
    attack_event(1180, AttackPayloadId::PigUltimateBomb),
    feedback_event(1440, FeedbackPhase::Aftermath, "recover_pig_ultimate_bomb"),
    timeline_event(1860, MoveTimelineEventKind::Recover),
];

const COMBO_FINISHER_EVENTS: [MoveTimelineEvent; 9] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_combo_finisher"),
    feedback_event(120, FeedbackPhase::PreHit, "trail_combo_finisher"),
    timeline_event(
        120,
        MoveTimelineEventKind::Motion {
            forward: 7.0,
            lift: 0.0,
        },
    ),
    timeline_event(
        250,
        MoveTimelineEventKind::Motion {
            forward: 5.0,
            lift: 0.0,
        },
    ),
    attack_event(310, AttackPayloadId::ComboFinisher),
    timeline_event(
        330,
        MoveTimelineEventKind::Motion {
            forward: 4.0,
            lift: 0.0,
        },
    ),
    timeline_event(430, MoveTimelineEventKind::Stop),
    feedback_event(450, FeedbackPhase::Aftermath, "combo_finisher_floor_pulse"),
    timeline_event(900, MoveTimelineEventKind::Recover),
];

const DASH_COMBO_FINISHER_EVENTS: [MoveTimelineEvent; 9] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_dash_combo_finisher"),
    timeline_event(
        0,
        MoveTimelineEventKind::Motion {
            forward: 7.0,
            lift: 0.0,
        },
    ),
    feedback_event(0, FeedbackPhase::PreHit, "trail_dash_combo_finisher"),
    attack_event(0, AttackPayloadId::DashComboFinisher),
    timeline_event(
        120,
        MoveTimelineEventKind::Motion {
            forward: 5.0,
            lift: 0.0,
        },
    ),
    timeline_event(
        250,
        MoveTimelineEventKind::Motion {
            forward: 4.0,
            lift: 0.0,
        },
    ),
    timeline_event(430, MoveTimelineEventKind::Stop),
    feedback_event(450, FeedbackPhase::Aftermath, "combo_finisher_floor_pulse"),
    timeline_event(900, MoveTimelineEventKind::Recover),
];

const DOG_COMBO_FINISHER_EVENTS: [MoveTimelineEvent; 9] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_dog_pounce"),
    feedback_event(110, FeedbackPhase::PreHit, "trail_dog_pounce"),
    timeline_event(
        115,
        MoveTimelineEventKind::Motion {
            forward: 6.2,
            lift: 0.0,
        },
    ),
    timeline_event(
        260,
        MoveTimelineEventKind::Motion {
            forward: 4.2,
            lift: 0.0,
        },
    ),
    attack_event(330, AttackPayloadId::DogBodyPounce),
    timeline_event(
        370,
        MoveTimelineEventKind::Motion {
            forward: 3.2,
            lift: 0.0,
        },
    ),
    timeline_event(500, MoveTimelineEventKind::Stop),
    feedback_event(610, FeedbackPhase::Aftermath, "recover_dog_pounce"),
    timeline_event(980, MoveTimelineEventKind::Recover),
];

const FOX_COMBO_FINISHER_EVENTS: [MoveTimelineEvent; 9] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_fox_tail_sweep"),
    feedback_event(80, FeedbackPhase::PreHit, "trail_fox_tail_sweep"),
    timeline_event(
        95,
        MoveTimelineEventKind::Motion {
            forward: 5.6,
            lift: 0.0,
        },
    ),
    timeline_event(
        190,
        MoveTimelineEventKind::Motion {
            forward: 3.6,
            lift: 0.0,
        },
    ),
    attack_event(235, AttackPayloadId::FoxTailSweep),
    timeline_event(
        270,
        MoveTimelineEventKind::Motion {
            forward: 2.2,
            lift: 0.0,
        },
    ),
    timeline_event(330, MoveTimelineEventKind::Stop),
    feedback_event(430, FeedbackPhase::Aftermath, "recover_fox_tail_sweep"),
    timeline_event(700, MoveTimelineEventKind::Recover),
];

const BEE_COMBO_FINISHER_EVENTS: [MoveTimelineEvent; 10] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_bee_spiral_sting"),
    feedback_event(95, FeedbackPhase::PreHit, "trail_bee_spiral_sting"),
    timeline_event(
        105,
        MoveTimelineEventKind::Motion {
            forward: 6.4,
            lift: 0.0,
        },
    ),
    timeline_event(
        210,
        MoveTimelineEventKind::Motion {
            forward: 4.4,
            lift: 0.0,
        },
    ),
    attack_event(260, AttackPayloadId::BeeSpiralSting),
    timeline_event(
        300,
        MoveTimelineEventKind::Motion {
            forward: 3.0,
            lift: 0.0,
        },
    ),
    bee_skill_event(315, BeeSkillId::WorkerSwarm),
    timeline_event(380, MoveTimelineEventKind::Stop),
    feedback_event(470, FeedbackPhase::Aftermath, "recover_bee_spiral_sting"),
    timeline_event(760, MoveTimelineEventKind::Recover),
];

const PENGUIN_COMBO_FINISHER_EVENTS: [MoveTimelineEvent; 10] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_penguin_belly_slide"),
    feedback_event(150, FeedbackPhase::PreHit, "trail_penguin_belly_slide"),
    timeline_event(
        170,
        MoveTimelineEventKind::Motion {
            forward: 6.6,
            lift: 0.0,
        },
    ),
    timeline_event(
        300,
        MoveTimelineEventKind::Motion {
            forward: 3.8,
            lift: 0.0,
        },
    ),
    penguin_skill_event(320, PenguinSkillId::IceTrail),
    attack_event(360, AttackPayloadId::PenguinBellySlide),
    timeline_event(
        430,
        MoveTimelineEventKind::Motion {
            forward: 2.4,
            lift: 0.0,
        },
    ),
    timeline_event(540, MoveTimelineEventKind::Stop),
    feedback_event(620, FeedbackPhase::Aftermath, "recover_penguin_belly_slide"),
    timeline_event(980, MoveTimelineEventKind::Recover),
];

const PANDA_COMBO_FINISHER_EVENTS: [MoveTimelineEvent; 9] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_panda_drop"),
    feedback_event(190, FeedbackPhase::PreHit, "trail_panda_drop"),
    timeline_event(
        210,
        MoveTimelineEventKind::Motion {
            forward: 5.0,
            lift: 0.0,
        },
    ),
    timeline_event(
        360,
        MoveTimelineEventKind::Motion {
            forward: 3.2,
            lift: 0.0,
        },
    ),
    attack_event(450, AttackPayloadId::PandaBodyDrop),
    timeline_event(
        500,
        MoveTimelineEventKind::Motion {
            forward: 2.2,
            lift: 0.0,
        },
    ),
    timeline_event(640, MoveTimelineEventKind::Stop),
    feedback_event(760, FeedbackPhase::Aftermath, "recover_panda_drop"),
    timeline_event(1120, MoveTimelineEventKind::Recover),
];

const PIG_COMBO_FINISHER_EVENTS: [MoveTimelineEvent; 9] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_pig_ham_slam"),
    feedback_event(250, FeedbackPhase::PreHit, "trail_pig_ham_slam"),
    timeline_event(
        280,
        MoveTimelineEventKind::Motion {
            forward: 4.4,
            lift: 0.0,
        },
    ),
    timeline_event(
        450,
        MoveTimelineEventKind::Motion {
            forward: 2.8,
            lift: 0.0,
        },
    ),
    attack_event(280, AttackPayloadId::PigHamSlam),
    timeline_event(
        640,
        MoveTimelineEventKind::Motion {
            forward: 1.4,
            lift: 0.0,
        },
    ),
    timeline_event(780, MoveTimelineEventKind::Stop),
    feedback_event(900, FeedbackPhase::Aftermath, "recover_pig_ham_slam"),
    timeline_event(1280, MoveTimelineEventKind::Recover),
];

const GRAB_EVENTS: [MoveTimelineEvent; 3] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_grab"),
    attack_event(180, AttackPayloadId::GrabCatch),
    timeline_event(640, MoveTimelineEventKind::Recover),
];

const DASH_ATTACK_EVENTS: [MoveTimelineEvent; 8] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_dash_attack"),
    timeline_event(
        55,
        MoveTimelineEventKind::Motion {
            forward: 0.85,
            lift: 0.0,
        },
    ),
    feedback_event(110, FeedbackPhase::PreHit, "trail_dash_attack"),
    attack_event(160, AttackPayloadId::DashStrike),
    attack_event(230, AttackPayloadId::DashShoulderBeat),
    feedback_event(360, FeedbackPhase::Aftermath, "dash_attack_brake"),
    timeline_event(500, MoveTimelineEventKind::NextTech),
    timeline_event(580, MoveTimelineEventKind::Recover),
];

const PENGUIN_DASH_ATTACK_EVENTS: [MoveTimelineEvent; 5] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_special_cast"),
    penguin_skill_event(0, PenguinSkillId::SnowflakeShot),
    feedback_event(90, FeedbackPhase::PreHit, "release_special_cast"),
    feedback_event(140, FeedbackPhase::Aftermath, "recover_special_cast"),
    timeline_event(160, MoveTimelineEventKind::Recover),
];

const PENGUIN_DASH_HEAVY_EVENTS: [MoveTimelineEvent; 2] = [
    penguin_skill_event(0, PenguinSkillId::SnowflakeShot),
    timeline_event(180, MoveTimelineEventKind::Recover),
];

const JUMP_ATTACK_EVENTS: [MoveTimelineEvent; 5] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_jump_attack"),
    attack_event(0, AttackPayloadId::JumpSpike),
    feedback_event(40, FeedbackPhase::PreHit, "trail_jump_attack"),
    feedback_event(180, FeedbackPhase::Aftermath, "jump_attack_fall"),
    timeline_event(520, MoveTimelineEventKind::Recover),
];

const JUMP_HEAVY_EVENTS: [MoveTimelineEvent; 4] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_jump_x_fish"),
    feedback_event(55, FeedbackPhase::PreHit, "release_jump_x_fish"),
    attack_event(90, AttackPayloadId::JumpFishShot),
    timeline_event(560, MoveTimelineEventKind::Recover),
];

const DOG_JUMP_ATTACK_EVENTS: [MoveTimelineEvent; 5] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_dog_jump_pounce"),
    attack_event(0, AttackPayloadId::DogJumpPounce),
    feedback_event(35, FeedbackPhase::PreHit, "trail_dog_jump_pounce"),
    feedback_event(210, FeedbackPhase::Aftermath, "dog_jump_pounce_fall"),
    timeline_event(560, MoveTimelineEventKind::Recover),
];

const DOG_JUMP_HEAVY_EVENTS: [MoveTimelineEvent; 4] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_dog_jump_fish"),
    feedback_event(70, FeedbackPhase::PreHit, "release_dog_jump_fish"),
    attack_event(115, AttackPayloadId::DogJumpFishShot),
    timeline_event(620, MoveTimelineEventKind::Recover),
];

const FOX_JUMP_ATTACK_EVENTS: [MoveTimelineEvent; 5] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_fox_jump_swipe"),
    attack_event(0, AttackPayloadId::FoxJumpSwipe),
    feedback_event(25, FeedbackPhase::PreHit, "trail_fox_jump_swipe"),
    feedback_event(135, FeedbackPhase::Aftermath, "fox_jump_swipe_fall"),
    timeline_event(410, MoveTimelineEventKind::Recover),
];

const FOX_JUMP_HEAVY_EVENTS: [MoveTimelineEvent; 4] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_fox_jump_fish"),
    feedback_event(45, FeedbackPhase::PreHit, "release_fox_jump_fish"),
    attack_event(70, AttackPayloadId::FoxJumpFishShot),
    timeline_event(480, MoveTimelineEventKind::Recover),
];

const BEE_JUMP_ATTACK_EVENTS: [MoveTimelineEvent; 5] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_bee_air_dash"),
    attack_event(0, AttackPayloadId::BeeAirSting),
    feedback_event(35, FeedbackPhase::PreHit, "trail_bee_air_dash"),
    feedback_event(165, FeedbackPhase::Aftermath, "bee_air_dash_glide"),
    timeline_event(420, MoveTimelineEventKind::Recover),
];

const BEE_JUMP_HEAVY_EVENTS: [MoveTimelineEvent; 5] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_bee_honey_glob"),
    bee_skill_event(0, BeeSkillId::HoneyGlob),
    feedback_event(70, FeedbackPhase::PreHit, "trail_bee_honey_glob"),
    feedback_event(260, FeedbackPhase::Aftermath, "recover_bee_honey_glob"),
    timeline_event(620, MoveTimelineEventKind::Recover),
];

const PENGUIN_JUMP_ATTACK_EVENTS: [MoveTimelineEvent; 5] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_special_cast"),
    penguin_skill_event(0, PenguinSkillId::SnowflakeShot),
    feedback_event(90, FeedbackPhase::PreHit, "release_special_cast"),
    feedback_event(140, FeedbackPhase::Aftermath, "recover_special_cast"),
    timeline_event(160, MoveTimelineEventKind::Recover),
];

const PENGUIN_JUMP_HEAVY_EVENTS: [MoveTimelineEvent; 5] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_special_cast"),
    penguin_skill_event(0, PenguinSkillId::SnowflakeSwapShot),
    feedback_event(35, FeedbackPhase::PreHit, "release_special_cast"),
    feedback_event(95, FeedbackPhase::Aftermath, "recover_special_cast"),
    timeline_event(140, MoveTimelineEventKind::Recover),
];

const CHICK_LIGHT1_EVENTS: [MoveTimelineEvent; 5] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_special_cast"),
    chick_skill_event(0, ChickSkillId::OrbitEggLaunch),
    feedback_event(70, FeedbackPhase::PreHit, "release_special_cast"),
    feedback_event(170, FeedbackPhase::Aftermath, "recover_special_cast"),
    timeline_event(260, MoveTimelineEventKind::Recover),
];

const CHICK_LIGHT2_EVENTS: [MoveTimelineEvent; 6] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_chick_sunny_flip"),
    feedback_event(110, FeedbackPhase::PreHit, "trail_chick_sunny_flip"),
    chick_skill_event(145, ChickSkillId::SunnyFlip),
    feedback_event(315, FeedbackPhase::Aftermath, "recover_chick_sunny_flip"),
    timeline_event(350, MoveTimelineEventKind::NextTech),
    timeline_event(460, MoveTimelineEventKind::Recover),
];

const CHICK_COMBO_FINISHER_EVENTS: [MoveTimelineEvent; 10] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_chick_shell_scramble"),
    timeline_event(
        80,
        MoveTimelineEventKind::Motion {
            forward: 5.2,
            lift: 0.0,
        },
    ),
    feedback_event(120, FeedbackPhase::PreHit, "trail_chick_shell_scramble"),
    chick_skill_event(145, ChickSkillId::ShellScramble),
    attack_event(165, AttackPayloadId::ChickShellScramble),
    timeline_event(
        245,
        MoveTimelineEventKind::Motion {
            forward: 2.6,
            lift: 0.0,
        },
    ),
    timeline_event(360, MoveTimelineEventKind::Stop),
    feedback_event(
        430,
        FeedbackPhase::Aftermath,
        "recover_chick_shell_scramble",
    ),
    timeline_event(610, MoveTimelineEventKind::NextTech),
    timeline_event(760, MoveTimelineEventKind::Recover),
];

const CHICK_HEAVY_EVENTS: [MoveTimelineEvent; 6] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_special_cast"),
    feedback_event(150, FeedbackPhase::PreHit, "release_special_cast"),
    chick_skill_event(180, ChickSkillId::OrbitEgg),
    feedback_event(420, FeedbackPhase::Aftermath, "recover_special_cast"),
    timeline_event(520, MoveTimelineEventKind::NextTech),
    timeline_event(720, MoveTimelineEventKind::Recover),
];

const CHICK_HEAVY2_EVENTS: [MoveTimelineEvent; 6] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_chick_eggplant_impostor"),
    feedback_event(
        230,
        FeedbackPhase::PreHit,
        "release_chick_eggplant_impostor",
    ),
    chick_skill_event(280, ChickSkillId::EggplantRoll),
    timeline_event(360, MoveTimelineEventKind::Stop),
    feedback_event(
        610,
        FeedbackPhase::Aftermath,
        "recover_chick_eggplant_impostor",
    ),
    timeline_event(900, MoveTimelineEventKind::Recover),
];

const CHICK_DASH_BACKSTEP_STOP_MS: u32 = 180;
const CHICK_DASH_BACKSTEP_RECOVER_MS: u32 = 300;

const CHICK_DASH_ATTACK_EVENTS: [MoveTimelineEvent; 4] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_chick_dash_backstep_c"),
    timeline_event(CHICK_DASH_BACKSTEP_STOP_MS, MoveTimelineEventKind::Stop),
    feedback_event(
        220,
        FeedbackPhase::Aftermath,
        "recover_chick_dash_backstep_c",
    ),
    timeline_event(
        CHICK_DASH_BACKSTEP_RECOVER_MS,
        MoveTimelineEventKind::Recover,
    ),
];

const CHICK_DASH_HEAVY_EVENTS: [MoveTimelineEvent; 4] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_chick_dash_backstep_x"),
    timeline_event(CHICK_DASH_BACKSTEP_STOP_MS, MoveTimelineEventKind::Stop),
    feedback_event(
        220,
        FeedbackPhase::Aftermath,
        "recover_chick_dash_backstep_x",
    ),
    timeline_event(
        CHICK_DASH_BACKSTEP_RECOVER_MS,
        MoveTimelineEventKind::Recover,
    ),
];

const CHICK_JUMP_ATTACK_EVENTS: [MoveTimelineEvent; 4] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_chick_updraft_glide"),
    feedback_event(120, FeedbackPhase::PreHit, "lift_chick_updraft_glide"),
    feedback_event(430, FeedbackPhase::Aftermath, "recover_chick_updraft_glide"),
    timeline_event(620, MoveTimelineEventKind::Recover),
];

const CHICK_JUMP_HEAVY_EVENTS: [MoveTimelineEvent; 5] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_chick_fresh_egg_drop"),
    chick_skill_event(0, ChickSkillId::FreshEggRide),
    feedback_event(75, FeedbackPhase::PreHit, "release_chick_fresh_egg_drop"),
    feedback_event(
        260,
        FeedbackPhase::Aftermath,
        "recover_chick_fresh_egg_drop",
    ),
    timeline_event(560, MoveTimelineEventKind::Recover),
];

const PANDA_JUMP_ATTACK_EVENTS: [MoveTimelineEvent; 5] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_panda_jump_drop"),
    attack_event(0, AttackPayloadId::PandaJumpDrop),
    feedback_event(55, FeedbackPhase::PreHit, "trail_panda_jump_drop"),
    feedback_event(260, FeedbackPhase::Aftermath, "panda_jump_drop_fall"),
    timeline_event(680, MoveTimelineEventKind::Recover),
];

const PANDA_JUMP_HEAVY_EVENTS: [MoveTimelineEvent; 4] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_panda_jump_fish"),
    feedback_event(85, FeedbackPhase::PreHit, "release_panda_jump_fish"),
    attack_event(140, AttackPayloadId::PandaJumpFishShot),
    timeline_event(720, MoveTimelineEventKind::Recover),
];

const PIG_JUMP_ATTACK_EVENTS: [MoveTimelineEvent; 5] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_jump_attack"),
    attack_event(0, AttackPayloadId::JumpSpike),
    feedback_event(40, FeedbackPhase::PreHit, "trail_jump_attack"),
    feedback_event(180, FeedbackPhase::Aftermath, "jump_attack_fall"),
    timeline_event(520, MoveTimelineEventKind::Recover),
];

const PIG_JUMP_HEAVY_EVENTS: [MoveTimelineEvent; 5] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_pig_air_meat_slam"),
    attack_event(0, AttackPayloadId::PigAirMeatSlam),
    feedback_event(90, FeedbackPhase::PreHit, "trail_pig_air_meat_slam"),
    feedback_event(300, FeedbackPhase::Aftermath, "recover_pig_air_meat_slam"),
    timeline_event(700, MoveTimelineEventKind::Recover),
];

const GUARD_COUNTER_EVENTS: [MoveTimelineEvent; 3] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_guard_counter"),
    attack_event(70, AttackPayloadId::GuardCounter),
    timeline_event(430, MoveTimelineEventKind::Recover),
];

const SPECIAL_CAST_EVENTS: [MoveTimelineEvent; 4] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_special_cast"),
    feedback_event(90, FeedbackPhase::PreHit, "release_special_cast"),
    feedback_event(260, FeedbackPhase::Aftermath, "recover_special_cast"),
    timeline_event(360, MoveTimelineEventKind::Recover),
];

const ITEM_PICKUP_EVENTS: [MoveTimelineEvent; 4] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_item_pickup"),
    timeline_event(
        80,
        MoveTimelineEventKind::Motion {
            forward: -0.03,
            lift: 0.0,
        },
    ),
    feedback_event(130, FeedbackPhase::Aftermath, "secure_item_pickup"),
    timeline_event(240, MoveTimelineEventKind::Recover),
];

const ITEM_DROP_EVENTS: [MoveTimelineEvent; 4] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_item_drop"),
    timeline_event(
        60,
        MoveTimelineEventKind::Motion {
            forward: 0.06,
            lift: 0.0,
        },
    ),
    feedback_event(120, FeedbackPhase::Aftermath, "release_item_drop"),
    timeline_event(220, MoveTimelineEventKind::Recover),
];

const GUARD_STEP_EVENTS: [MoveTimelineEvent; 4] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_guard_step"),
    timeline_event(
        45,
        MoveTimelineEventKind::Motion {
            forward: -0.08,
            lift: 0.0,
        },
    ),
    feedback_event(130, FeedbackPhase::Aftermath, "guard_step_recover"),
    timeline_event(260, MoveTimelineEventKind::Recover),
];

const QUICK_STAND_EVENTS: [MoveTimelineEvent; 4] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_quick_stand"),
    timeline_event(
        70,
        MoveTimelineEventKind::Motion {
            forward: -0.04,
            lift: 0.0,
        },
    ),
    feedback_event(150, FeedbackPhase::Aftermath, "quick_stand_ready"),
    timeline_event(240, MoveTimelineEventKind::Recover),
];

const RECOVERY_ROLL_EVENTS: [MoveTimelineEvent; 3] = [
    feedback_event(0, FeedbackPhase::Startup, "startup_recovery_roll"),
    feedback_event(90, FeedbackPhase::PreHit, "travel_recovery_roll"),
    timeline_event(360, MoveTimelineEventKind::Recover),
];

const LANDING_RECOVERY_EVENTS: [MoveTimelineEvent; 4] = [
    feedback_event(0, FeedbackPhase::Startup, "landing_recovery_stick"),
    timeline_event(
        55,
        MoveTimelineEventKind::Motion {
            forward: -0.04,
            lift: 0.0,
        },
    ),
    feedback_event(135, FeedbackPhase::Aftermath, "landing_recovery_release"),
    timeline_event(220, MoveTimelineEventKind::Recover),
];

const EMPTY_EVENTS: [MoveTimelineEvent; 0] = [];

const AUTHORED_TECHNIQUE_ORDER: &[TechniqueId] = &[
    TechniqueId::CatLight2,
    TechniqueId::CatLight1,
    TechniqueId::CatHeavy2,
    TechniqueId::CatHeavy,
    TechniqueId::CatUltimateStartup,
    TechniqueId::CatUltimateRush,
    TechniqueId::Grab,
    TechniqueId::CatDashAttack,
    TechniqueId::CatJumpHeavy,
    TechniqueId::CatJumpAttack,
    TechniqueId::GuardCounter,
    TechniqueId::CatComboFinisher,
    TechniqueId::BeeLight2,
    TechniqueId::BeeLight1,
    TechniqueId::BeeHeavy2,
    TechniqueId::BeeHeavy,
    TechniqueId::BeeUltimateStartup,
    TechniqueId::BeeLegacyUltimateStartup,
    TechniqueId::BeeLegacyUltimateRush,
    TechniqueId::BeeDashAttack,
    TechniqueId::BeeJumpHeavy,
    TechniqueId::BeeJumpAttack,
    TechniqueId::BeeComboFinisher,
    TechniqueId::PenguinLight2,
    TechniqueId::PenguinLight1,
    TechniqueId::PenguinHeavy2,
    TechniqueId::PenguinHeavy,
    TechniqueId::PenguinUltimateStartup,
    TechniqueId::PenguinUltimateRush,
    TechniqueId::PenguinDashAttack,
    TechniqueId::PenguinDashHeavy,
    TechniqueId::PenguinJumpHeavy,
    TechniqueId::PenguinJumpAttack,
    TechniqueId::PenguinComboFinisher,
    TechniqueId::ChickLight2,
    TechniqueId::ChickLight1,
    TechniqueId::ChickHeavy2,
    TechniqueId::ChickHeavy,
    TechniqueId::ChickUltimateStartup,
    TechniqueId::ChickDashAttack,
    TechniqueId::ChickDashHeavy,
    TechniqueId::ChickJumpHeavy,
    TechniqueId::ChickJumpAttack,
    TechniqueId::ChickComboFinisher,
    TechniqueId::SpecialCast,
    TechniqueId::ItemPickup,
    TechniqueId::ItemSwing,
    TechniqueId::ItemThrow,
    TechniqueId::ItemDrop,
    TechniqueId::GuardStep,
    TechniqueId::QuickStand,
    TechniqueId::RecoveryRoll,
    TechniqueId::LandingRecovery,
];

pub const fn ms_to_secs(ms: u32) -> f32 {
    ms as f32 / MS_PER_SECOND
}

pub fn elapsed_secs_to_ms(elapsed: f32) -> u32 {
    (elapsed.max(0.0) * MS_PER_SECOND).floor() as u32
}

pub const fn timeline_event(at_ms: u32, kind: MoveTimelineEventKind) -> MoveTimelineEvent {
    MoveTimelineEvent { at_ms, kind }
}

pub const fn attack_event(at_ms: u32, payload_id: AttackPayloadId) -> MoveTimelineEvent {
    timeline_event(at_ms, MoveTimelineEventKind::Attack(payload_id))
}

pub const fn charged_attack_event(
    at_ms: u32,
    tap: AttackPayloadId,
    partial: AttackPayloadId,
    full: AttackPayloadId,
) -> MoveTimelineEvent {
    timeline_event(
        at_ms,
        MoveTimelineEventKind::ChargedAttack { tap, partial, full },
    )
}

pub const fn bee_skill_event(at_ms: u32, skill_id: BeeSkillId) -> MoveTimelineEvent {
    timeline_event(at_ms, MoveTimelineEventKind::SpawnBeeSkill(skill_id))
}

pub const fn penguin_skill_event(at_ms: u32, skill_id: PenguinSkillId) -> MoveTimelineEvent {
    timeline_event(at_ms, MoveTimelineEventKind::SpawnPenguinSkill(skill_id))
}

pub const fn chick_skill_event(at_ms: u32, skill_id: ChickSkillId) -> MoveTimelineEvent {
    timeline_event(at_ms, MoveTimelineEventKind::SpawnChickSkill(skill_id))
}

pub const fn feedback_event(
    at_ms: u32,
    phase: FeedbackPhase,
    cue: &'static str,
) -> MoveTimelineEvent {
    timeline_event(at_ms, MoveTimelineEventKind::Feedback(phase, cue))
}

pub const fn damage_modifier(
    condition: DamageCondition,
    scale: f32,
    add: f32,
) -> DamageModifierDef {
    DamageModifierDef {
        condition,
        scale,
        add,
    }
}

pub const fn damage_reduction(condition: DamageCondition, factor: f32) -> DamageReductionDef {
    DamageReductionDef { condition, factor }
}

pub const fn damage_terminal(
    kind: DamageTerminalKind,
    ignore_time_ms: u32,
    score_scale: f32,
) -> DamageTerminalDef {
    DamageTerminalDef {
        kind,
        ignore_time_ms,
        score_scale,
    }
}

pub const fn damage_terminal_override(
    condition: DamageCondition,
    kind: DamageTerminalKind,
    ignore_time_ms: u32,
    score_scale: f32,
) -> DamageTerminalOverrideDef {
    DamageTerminalOverrideDef {
        condition,
        terminal: damage_terminal(kind, ignore_time_ms, score_scale),
    }
}

pub const fn damage_side_effect(
    condition: DamageCondition,
    id: DamageSideEffectId,
    cue: &'static str,
    invulnerability_ms: u32,
    stamina_delta: f32,
    hud_flash: f32,
    score_scale_add: f32,
) -> DamageSideEffectDef {
    DamageSideEffectDef {
        condition,
        id,
        cue,
        invulnerability_ms,
        stamina_delta,
        hud_flash,
        score_scale_add,
    }
}

pub fn attack_shape_definition(id: AttackShapeId) -> AttackShapeDef {
    match id {
        AttackShapeId::CompactSlashLead => AttackShapeDef {
            id,
            range: COMPACT_HOOK_EDGE_RANGE,
            radius: COMPACT_HOOK_EDGE_RADIUS,
            vertical_offset_scale: 0.58,
            parented: true,
            curved: false,
            effect_type: 3,
            path: &COMPACT_SLASH_LEAD_PATH,
        },
        AttackShapeId::CompactSlashFollow => AttackShapeDef {
            id,
            range: COMPACT_HOOK_EDGE_RANGE,
            radius: COMPACT_HOOK_EDGE_RADIUS,
            vertical_offset_scale: 0.58,
            parented: true,
            curved: false,
            effect_type: 3,
            path: &COMPACT_SLASH_FOLLOW_PATH,
        },
        AttackShapeId::CompactSlashTight => AttackShapeDef {
            id,
            range: COMBO_FINISHER_RANGE,
            radius: COMBO_FINISHER_RADIUS,
            vertical_offset_scale: 0.58,
            parented: true,
            curved: false,
            effect_type: 6,
            path: &COMPACT_SLASH_TIGHT_PATH,
        },
        AttackShapeId::LauncherRiser => AttackShapeDef {
            id,
            range: HEAVY_RANGE,
            radius: HEAVY_RADIUS,
            vertical_offset_scale: 0.5,
            parented: false,
            curved: false,
            effect_type: 3,
            path: &LAUNCHER_RISER_PATH,
        },
        AttackShapeId::BodyRoll => AttackShapeDef {
            id,
            range: COMBO_FINISHER_RANGE,
            radius: COMBO_FINISHER_RADIUS,
            vertical_offset_scale: 0.52,
            parented: true,
            curved: false,
            effect_type: 3,
            path: &BODY_ROLL_PATH,
        },
        AttackShapeId::CompactThrust => AttackShapeDef {
            id,
            range: COMBO_FINISHER_RANGE,
            radius: COMBO_FINISHER_RADIUS,
            vertical_offset_scale: 0.56,
            parented: true,
            curved: false,
            effect_type: 6,
            path: &COMPACT_THRUST_PATH,
        },
        AttackShapeId::DelayedRiser => AttackShapeDef {
            id,
            range: HEAVY_RANGE * 0.72,
            radius: HEAVY_RADIUS,
            vertical_offset_scale: 0.5,
            parented: false,
            curved: false,
            effect_type: 3,
            path: &DELAYED_RISER_PATH,
        },
        AttackShapeId::SweepingArcWide => AttackShapeDef {
            id,
            range: LIGHT2_RANGE * 1.08,
            radius: LIGHT2_RADIUS * 0.9,
            vertical_offset_scale: 0.58,
            parented: true,
            curved: true,
            effect_type: 3,
            path: &SWEEPING_ARC_WIDE_PATH,
        },
        AttackShapeId::HookSweep => AttackShapeDef {
            id,
            range: LIGHT2_RANGE,
            radius: LIGHT2_RADIUS * 0.86,
            vertical_offset_scale: 0.56,
            parented: true,
            curved: true,
            effect_type: 3,
            path: &HOOK_SWEEP_PATH,
        },
        AttackShapeId::RisingColumn => AttackShapeDef {
            id,
            range: HEAVY_RANGE * 0.88,
            radius: HEAVY_RADIUS * 0.82,
            vertical_offset_scale: 0.44,
            parented: false,
            curved: true,
            effect_type: 3,
            path: &RISING_COLUMN_PATH,
        },
        AttackShapeId::FallingSpikeArc => AttackShapeDef {
            id,
            range: LIGHT_RANGE,
            radius: LIGHT_RADIUS * 1.2,
            vertical_offset_scale: 0.48,
            parented: true,
            curved: true,
            effect_type: 3,
            path: &FALLING_SPIKE_ARC_PATH,
        },
        AttackShapeId::AirFishShot => AttackShapeDef {
            id,
            range: FIGHTER_RADIUS * 1.15,
            radius: JUMP_HEAVY_FISH_RADIUS,
            vertical_offset_scale: 0.6,
            parented: false,
            curved: false,
            effect_type: 3,
            path: &AIR_FISH_SHOT_PATH,
        },
        AttackShapeId::CatPounceCatch => AttackShapeDef {
            id,
            range: CAT_POUNCE_CATCH_RANGE,
            radius: CAT_POUNCE_CATCH_RADIUS,
            vertical_offset_scale: 0.54,
            parented: true,
            curved: false,
            effect_type: 3,
            path: &CAT_POUNCE_CATCH_PATH,
        },
        AttackShapeId::UltimateCatch => AttackShapeDef {
            id,
            range: ULTIMATE_CATCH_RANGE,
            radius: ULTIMATE_CATCH_RADIUS,
            vertical_offset_scale: 0.58,
            parented: true,
            curved: false,
            effect_type: 3,
            path: &ULTIMATE_CATCH_PATH,
        },
        AttackShapeId::UltimateScratchLeft => AttackShapeDef {
            id,
            range: ULTIMATE_SCRATCH_RANGE,
            radius: ULTIMATE_SCRATCH_RADIUS,
            vertical_offset_scale: 0.6,
            parented: true,
            curved: true,
            effect_type: 3,
            path: &ULTIMATE_SCRATCH_LEFT_PATH,
        },
        AttackShapeId::UltimateScratchRight => AttackShapeDef {
            id,
            range: ULTIMATE_SCRATCH_RANGE,
            radius: ULTIMATE_SCRATCH_RADIUS,
            vertical_offset_scale: 0.6,
            parented: true,
            curved: true,
            effect_type: 3,
            path: &ULTIMATE_SCRATCH_RIGHT_PATH,
        },
        AttackShapeId::UltimateBomb => AttackShapeDef {
            id,
            range: ULTIMATE_LOCK_DISTANCE,
            radius: ULTIMATE_BOMB_RADIUS,
            vertical_offset_scale: 0.5,
            parented: true,
            curved: true,
            effect_type: 6,
            path: &ULTIMATE_BOMB_PATH,
        },
        AttackShapeId::ShoulderLine => AttackShapeDef {
            id,
            range: DASH_ATTACK_RANGE * 1.08,
            radius: DASH_ATTACK_RADIUS * 0.84,
            vertical_offset_scale: 0.55,
            parented: true,
            curved: false,
            effect_type: 3,
            path: &SHOULDER_LINE_PATH,
        },
        AttackShapeId::CatBodySkid => AttackShapeDef {
            id,
            range: CAT_BODY_SKID_RANGE,
            radius: CAT_BODY_SKID_RADIUS,
            vertical_offset_scale: 0.47,
            parented: true,
            curved: true,
            effect_type: 3,
            path: &CAT_BODY_SKID_PATH,
        },
        AttackShapeId::GroundSkid => AttackShapeDef {
            id,
            range: GROUND_SKID_BODY_RANGE,
            radius: GROUND_SKID_BODY_RADIUS,
            vertical_offset_scale: 0.5,
            parented: false,
            curved: true,
            effect_type: 3,
            path: &GROUND_SKID_PATH,
        },
        AttackShapeId::PenguinSlopeBody => AttackShapeDef {
            id,
            range: PENGUIN_SLOPE_BODY_RANGE,
            radius: PENGUIN_SLOPE_BODY_RADIUS,
            vertical_offset_scale: 0.33,
            parented: true,
            curved: true,
            effect_type: 3,
            path: &PENGUIN_SLOPE_BODY_PATH,
        },
        AttackShapeId::PenguinUltimateSlopeBody => AttackShapeDef {
            id,
            range: PENGUIN_ULTIMATE_SLOPE_BODY_RANGE,
            radius: PENGUIN_ULTIMATE_SLOPE_BODY_RADIUS,
            vertical_offset_scale: 0.24,
            parented: true,
            curved: true,
            effect_type: 3,
            path: &PENGUIN_SLOPE_BODY_PATH,
        },
        AttackShapeId::PigBodyShove => AttackShapeDef {
            id,
            range: FIGHTER_RADIUS * 1.1,
            radius: FIGHTER_RADIUS * 0.68,
            vertical_offset_scale: 0.5,
            parented: true,
            curved: false,
            effect_type: 3,
            path: &PIG_BODY_SHOVE_PATH,
        },
        AttackShapeId::PigBellyCrash => AttackShapeDef {
            id,
            range: FIGHTER_RADIUS * 1.25,
            radius: FIGHTER_RADIUS * 0.88,
            vertical_offset_scale: 0.48,
            parented: true,
            curved: true,
            effect_type: 6,
            path: &PIG_BELLY_CRASH_PATH,
        },
        AttackShapeId::PigRollingPinLine => AttackShapeDef {
            id,
            range: HEAVY_RANGE * 0.82,
            radius: HEAVY_RADIUS * 0.9,
            vertical_offset_scale: 0.52,
            parented: true,
            curved: false,
            effect_type: 3,
            path: &PIG_ROLLING_PIN_LINE_PATH,
        },
        AttackShapeId::PigHamLob => AttackShapeDef {
            id,
            range: FIGHTER_RADIUS * 0.9,
            radius: JUMP_HEAVY_FISH_RADIUS * 1.18,
            vertical_offset_scale: 0.58,
            parented: false,
            curved: true,
            effect_type: 3,
            path: &PIG_HAM_LOB_PATH,
        },
        AttackShapeId::PigHalfCircleSwing => AttackShapeDef {
            id,
            range: FIGHTER_RADIUS * 1.18,
            radius: HEAVY_RADIUS * 0.58,
            vertical_offset_scale: 0.58,
            parented: true,
            curved: true,
            effect_type: 3,
            path: &PIG_HALF_CIRCLE_SWING_PATH,
        },
        AttackShapeId::PigMeatSlam => AttackShapeDef {
            id,
            range: FIGHTER_RADIUS * 0.92,
            radius: COMBO_FINISHER_RADIUS * 0.74,
            vertical_offset_scale: 0.5,
            parented: true,
            curved: true,
            effect_type: 6,
            path: &PIG_MEAT_SLAM_PATH,
        },
        AttackShapeId::PigAirMeatSlam => AttackShapeDef {
            id,
            range: FIGHTER_RADIUS * 0.72,
            radius: JUMP_HEAVY_FISH_RADIUS * 1.34,
            vertical_offset_scale: 0.42,
            parented: true,
            curved: true,
            effect_type: 3,
            path: &PIG_AIR_MEAT_SLAM_PATH,
        },
        AttackShapeId::PigUltimateGrab => AttackShapeDef {
            id,
            range: ULTIMATE_CATCH_RANGE * 0.92,
            radius: ULTIMATE_CATCH_RADIUS * 1.08,
            vertical_offset_scale: 0.58,
            parented: true,
            curved: false,
            effect_type: 3,
            path: &PIG_ULTIMATE_GRAB_PATH,
        },
        AttackShapeId::CurvedLob => AttackShapeDef {
            id,
            range: 0.0,
            radius: 0.5,
            vertical_offset_scale: 0.5,
            parented: false,
            curved: true,
            effect_type: 0,
            path: &CURVED_LOB_PATH,
        },
        AttackShapeId::CounterArc => AttackShapeDef {
            id,
            range: GUARD_COUNTER_RANGE,
            radius: GUARD_COUNTER_RADIUS,
            vertical_offset_scale: 0.56,
            parented: true,
            curved: false,
            effect_type: 3,
            path: &COUNTER_ARC_PATH,
        },
        AttackShapeId::ProjectileBolt => AttackShapeDef {
            id,
            range: 0.0,
            radius: SPECIAL_PROJECTILE_RADIUS,
            vertical_offset_scale: 0.5,
            parented: false,
            curved: false,
            effect_type: 3,
            path: &PROJECTILE_BOLT_PATH,
        },
        AttackShapeId::TrapPlate => AttackShapeDef {
            id,
            range: 0.0,
            radius: SPECIAL_TRAP_RADIUS,
            vertical_offset_scale: 0.04,
            parented: false,
            curved: false,
            effect_type: 0,
            path: &TRAP_PLATE_PATH,
        },
        AttackShapeId::ShockwaveRing => AttackShapeDef {
            id,
            range: 0.0,
            radius: SPECIAL_SHOCKWAVE_RADIUS,
            vertical_offset_scale: 0.06,
            parented: false,
            curved: true,
            effect_type: 0,
            path: &SHOCKWAVE_RING_PATH,
        },
        AttackShapeId::HazardField => AttackShapeDef {
            id,
            range: 0.0,
            radius: SPECIAL_HAZARD_RADIUS,
            vertical_offset_scale: 0.08,
            parented: false,
            curved: true,
            effect_type: 0,
            path: &HAZARD_FIELD_PATH,
        },
        AttackShapeId::ItemLob => AttackShapeDef {
            id,
            range: 0.0,
            radius: ITEM_THROW_RADIUS,
            vertical_offset_scale: 0.45,
            parented: false,
            curved: true,
            effect_type: 0,
            path: &ITEM_LOB_PATH,
        },
        AttackShapeId::BombBurst => AttackShapeDef {
            id,
            range: 0.0,
            radius: POP_BOMB_RADIUS,
            vertical_offset_scale: 0.36,
            parented: false,
            curved: true,
            effect_type: 0,
            path: &BOMB_BURST_PATH,
        },
        AttackShapeId::GrabCatch => AttackShapeDef {
            id,
            range: GRAB_RANGE,
            radius: GRAB_RADIUS,
            vertical_offset_scale: 0.58,
            parented: true,
            curved: false,
            effect_type: 0,
            path: &GRAB_CATCH_PATH,
        },
        AttackShapeId::DashShoulder => AttackShapeDef {
            id,
            range: DASH_ATTACK_RANGE,
            radius: DASH_ATTACK_RADIUS,
            vertical_offset_scale: 0.56,
            parented: true,
            curved: false,
            effect_type: 3,
            path: &DASH_SHOULDER_PATH,
        },
        AttackShapeId::JumpKick => AttackShapeDef {
            id,
            range: JUMP_ATTACK_RANGE,
            radius: JUMP_ATTACK_RADIUS,
            vertical_offset_scale: 0.42,
            parented: true,
            curved: false,
            effect_type: 3,
            path: &JUMP_KICK_PATH,
        },
        AttackShapeId::ItemMelee => AttackShapeDef {
            id,
            range: ITEM_MALLET_RANGE,
            radius: ITEM_MALLET_RADIUS,
            vertical_offset_scale: 0.62,
            parented: true,
            curved: false,
            effect_type: 3,
            path: &ITEM_MELEE_PATH,
        },
    }
}

pub fn damage_profile_definition(id: DamageProfileId) -> DamageProfileDef {
    match id {
        DamageProfileId::Direct => DamageProfileDef {
            id,
            defense_mode: DamageDefenseMode::Normal { factor: 1.0 },
            reductions: NO_DAMAGE_REDUCTIONS,
            modifiers: DIRECT_DAMAGE_MODIFIERS,
            side_effects: DIRECT_DAMAGE_SIDE_EFFECTS,
            terminal: damage_terminal(DamageTerminalKind::Normal, 0, 1.0),
            terminal_overrides: ELEMENT_ABSORB_TERMINALS,
            guard_stamina_scale: 1.0,
            minimum_damage: 1.0,
        },
        DamageProfileId::BasicStrike => DamageProfileDef {
            id,
            defense_mode: DamageDefenseMode::Normal { factor: 1.0 },
            reductions: NO_DAMAGE_REDUCTIONS,
            modifiers: BASIC_STRIKE_DAMAGE_MODIFIERS,
            side_effects: BASIC_STRIKE_DAMAGE_SIDE_EFFECTS,
            terminal: damage_terminal(DamageTerminalKind::Normal, 0, 1.0),
            terminal_overrides: ELEMENT_ABSORB_TERMINALS,
            guard_stamina_scale: 0.92,
            minimum_damage: 1.0,
        },
        DamageProfileId::FollowupStrike => DamageProfileDef {
            id,
            defense_mode: DamageDefenseMode::Normal { factor: 1.0 },
            reductions: NO_DAMAGE_REDUCTIONS,
            modifiers: FOLLOWUP_DAMAGE_MODIFIERS,
            side_effects: FOLLOWUP_DAMAGE_SIDE_EFFECTS,
            terminal: damage_terminal(DamageTerminalKind::Normal, 0, 1.0),
            terminal_overrides: ELEMENT_ABSORB_TERMINALS,
            guard_stamina_scale: 1.0,
            minimum_damage: 1.0,
        },
        DamageProfileId::LauncherCommit => DamageProfileDef {
            id,
            defense_mode: DamageDefenseMode::Normal { factor: 1.0 },
            reductions: COMMITTED_MOVE_REDUCTIONS,
            modifiers: LAUNCHER_DAMAGE_MODIFIERS,
            side_effects: LAUNCHER_DAMAGE_SIDE_EFFECTS,
            terminal: damage_terminal(DamageTerminalKind::Normal, 0, 1.0),
            terminal_overrides: ELEMENT_ABSORB_TERMINALS,
            guard_stamina_scale: 1.18,
            minimum_damage: 1.0,
        },
        DamageProfileId::GroundBounce => DamageProfileDef {
            id,
            defense_mode: DamageDefenseMode::Normal { factor: 1.0 },
            reductions: BOUNCE_MOVE_REDUCTIONS,
            modifiers: GROUND_BOUNCE_DAMAGE_MODIFIERS,
            side_effects: GROUND_BOUNCE_DAMAGE_SIDE_EFFECTS,
            terminal: damage_terminal(DamageTerminalKind::Normal, 0, 1.0),
            terminal_overrides: ELEMENT_ABSORB_TERMINALS,
            guard_stamina_scale: 1.12,
            minimum_damage: 1.0,
        },
        DamageProfileId::GrabControl => DamageProfileDef {
            id,
            defense_mode: DamageDefenseMode::Ignore,
            reductions: NO_DAMAGE_REDUCTIONS,
            modifiers: GRAB_DAMAGE_MODIFIERS,
            side_effects: GRAB_DAMAGE_SIDE_EFFECTS,
            terminal: damage_terminal(DamageTerminalKind::Normal, 0, 1.0),
            terminal_overrides: NO_DAMAGE_TERMINAL_OVERRIDES,
            guard_stamina_scale: 0.0,
            minimum_damage: 1.0,
        },
        DamageProfileId::DashBody => DamageProfileDef {
            id,
            defense_mode: DamageDefenseMode::Normal { factor: 1.0 },
            reductions: NO_DAMAGE_REDUCTIONS,
            modifiers: DASH_BODY_DAMAGE_MODIFIERS,
            side_effects: DASH_BODY_DAMAGE_SIDE_EFFECTS,
            terminal: damage_terminal(DamageTerminalKind::Normal, 0, 1.0),
            terminal_overrides: ELEMENT_ABSORB_TERMINALS,
            guard_stamina_scale: 1.1,
            minimum_damage: 1.0,
        },
        DamageProfileId::AerialSpike => DamageProfileDef {
            id,
            defense_mode: DamageDefenseMode::Normal { factor: 1.0 },
            reductions: NO_DAMAGE_REDUCTIONS,
            modifiers: AERIAL_SPIKE_DAMAGE_MODIFIERS,
            side_effects: AERIAL_SPIKE_DAMAGE_SIDE_EFFECTS,
            terminal: damage_terminal(DamageTerminalKind::Normal, 0, 1.0),
            terminal_overrides: ELEMENT_ABSORB_TERMINALS,
            guard_stamina_scale: 1.08,
            minimum_damage: 1.0,
        },
        DamageProfileId::CounterBlow => DamageProfileDef {
            id,
            defense_mode: DamageDefenseMode::Fixed { factor: 1.0 },
            reductions: NO_DAMAGE_REDUCTIONS,
            modifiers: COUNTER_BLOW_DAMAGE_MODIFIERS,
            side_effects: COUNTER_BLOW_DAMAGE_SIDE_EFFECTS,
            terminal: damage_terminal(DamageTerminalKind::Normal, 0, 1.0),
            terminal_overrides: ELEMENT_ABSORB_TERMINALS,
            guard_stamina_scale: 1.26,
            minimum_damage: 1.0,
        },
        DamageProfileId::UltimateRush => DamageProfileDef {
            id,
            defense_mode: DamageDefenseMode::Fixed { factor: 1.0 },
            reductions: NO_DAMAGE_REDUCTIONS,
            modifiers: NO_DAMAGE_MODIFIERS,
            side_effects: NO_DAMAGE_SIDE_EFFECTS,
            terminal: damage_terminal(DamageTerminalKind::Normal, 0, 1.0),
            terminal_overrides: ELEMENT_ABSORB_TERMINALS,
            guard_stamina_scale: 1.4,
            minimum_damage: 1.0,
        },
        DamageProfileId::ItemHeavy => DamageProfileDef {
            id,
            defense_mode: DamageDefenseMode::Fixed { factor: 0.95 },
            reductions: NO_DAMAGE_REDUCTIONS,
            modifiers: ITEM_HEAVY_DAMAGE_MODIFIERS,
            side_effects: ITEM_HEAVY_DAMAGE_SIDE_EFFECTS,
            terminal: damage_terminal(DamageTerminalKind::Normal, 200, 1.0),
            terminal_overrides: ELEMENT_ABSORB_TERMINALS,
            guard_stamina_scale: 1.16,
            minimum_damage: 1.0,
        },
    }
}

pub fn attack_payload_definition(id: AttackPayloadId) -> AttackPayloadDef {
    match id {
        AttackPayloadId::AsBeat1 => AttackPayloadDef {
            id,
            kind: AttackKind::Light1,
            shape_id: AttackShapeId::CompactSlashLead,
            reaction_family: ReactionFamilyId::ShortStandingStagger,
            damage_profile: DamageProfileId::BasicStrike,
            element: DamageElement::Strike,
            power: 9.0,
            str_scale: 0.8,
            time_ms: 100,
            damage: LIGHT_DAMAGE,
            knockback: LIGHT_KNOCKBACK,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_A_s_1",
            hitstop_scale: 0.95,
            shake_scale: 0.9,
            feedback_priority_bonus: 0,
        },
        AttackPayloadId::AsBeat2 => AttackPayloadDef {
            id,
            kind: AttackKind::Light1,
            shape_id: AttackShapeId::CompactSlashFollow,
            reaction_family: ReactionFamilyId::ShortStandingStagger,
            damage_profile: DamageProfileId::BasicStrike,
            element: DamageElement::Strike,
            power: 9.0,
            str_scale: 0.8,
            time_ms: 100,
            damage: LIGHT_DAMAGE,
            knockback: LIGHT_KNOCKBACK * 0.92,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_A_s_2",
            hitstop_scale: 1.0,
            shake_scale: 1.0,
            feedback_priority_bonus: 1,
        },
        AttackPayloadId::AssBeat1 => AttackPayloadDef {
            id,
            kind: AttackKind::Light2,
            shape_id: AttackShapeId::CompactSlashFollow,
            reaction_family: ReactionFamilyId::ShortStandingStagger,
            damage_profile: DamageProfileId::BasicStrike,
            element: DamageElement::Strike,
            power: 9.0,
            str_scale: 0.8,
            time_ms: 100,
            damage: LIGHT_DAMAGE,
            knockback: LIGHT_KNOCKBACK,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_A_ss_1",
            hitstop_scale: 0.95,
            shake_scale: 0.9,
            feedback_priority_bonus: 2,
        },
        AttackPayloadId::AssBeat2 => AttackPayloadDef {
            id,
            kind: AttackKind::Light2,
            shape_id: AttackShapeId::HookSweep,
            reaction_family: ReactionFamilyId::ShortStandingStagger,
            damage_profile: DamageProfileId::FollowupStrike,
            element: DamageElement::Strike,
            power: 10.0,
            str_scale: 0.8,
            time_ms: 100,
            damage: LIGHT2_DAMAGE,
            knockback: LIGHT2_KNOCKBACK * 0.92,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_A_ss_2",
            hitstop_scale: 1.08,
            shake_scale: 1.1,
            feedback_priority_bonus: 3,
        },
        AttackPayloadId::HeavyStep => AttackPayloadDef {
            id,
            kind: AttackKind::Heavy,
            shape_id: AttackShapeId::ShoulderLine,
            reaction_family: ReactionFamilyId::HeavyStandingStagger,
            damage_profile: DamageProfileId::BasicStrike,
            element: DamageElement::Strike,
            power: 11.0,
            str_scale: 0.8,
            time_ms: 120,
            damage: HEAVY_DAMAGE * 0.45,
            knockback: HEAVY_KNOCKBACK * 0.16,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_heavy_step",
            hitstop_scale: 1.12,
            shake_scale: 1.08,
            feedback_priority_bonus: 4,
        },
        AttackPayloadId::KiriageBeat1 => AttackPayloadDef {
            id,
            kind: AttackKind::Heavy,
            shape_id: AttackShapeId::DelayedRiser,
            reaction_family: ReactionFamilyId::LauncherDown,
            damage_profile: DamageProfileId::LauncherCommit,
            element: DamageElement::Launch,
            power: 15.0,
            str_scale: 0.8,
            time_ms: 100,
            damage: HEAVY_DAMAGE * 0.55,
            knockback: HEAVY_KNOCKBACK * 0.62,
            vertical_knockback: 6.5,
            guardable: true,
            impact_cue: "impact_kiriage_lift",
            hitstop_scale: 1.2,
            shake_scale: 1.15,
            feedback_priority_bonus: 5,
        },
        AttackPayloadId::KiriageBeat2 => AttackPayloadDef {
            id,
            kind: AttackKind::Heavy,
            shape_id: AttackShapeId::RisingColumn,
            reaction_family: ReactionFamilyId::LauncherDown,
            damage_profile: DamageProfileId::LauncherCommit,
            element: DamageElement::Launch,
            power: 15.0,
            str_scale: 0.8,
            time_ms: 100,
            damage: HEAVY_DAMAGE,
            knockback: HEAVY_KNOCKBACK * 0.72,
            vertical_knockback: 7.2,
            guardable: true,
            impact_cue: "impact_kiriage_launch",
            hitstop_scale: 1.32,
            shake_scale: 1.28,
            feedback_priority_bonus: 8,
        },
        AttackPayloadId::UltimateCatch => AttackPayloadDef {
            id,
            kind: AttackKind::Ultimate,
            shape_id: AttackShapeId::CatPounceCatch,
            reaction_family: ReactionFamilyId::HeavyStandingStagger,
            damage_profile: DamageProfileId::UltimateRush,
            element: DamageElement::Strike,
            power: 11.0,
            str_scale: 0.8,
            time_ms: 350,
            damage: ULTIMATE_CATCH_DAMAGE,
            knockback: 1.4,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_ultimate_catch",
            hitstop_scale: 1.12,
            shake_scale: 1.04,
            feedback_priority_bonus: 8,
        },
        AttackPayloadId::UltimateScratchLight => AttackPayloadDef {
            id,
            kind: AttackKind::Ultimate,
            shape_id: AttackShapeId::UltimateScratchLeft,
            reaction_family: ReactionFamilyId::UltimateLockedStagger,
            damage_profile: DamageProfileId::UltimateRush,
            element: DamageElement::Strike,
            power: 11.0,
            str_scale: 0.8,
            time_ms: 100,
            damage: ULTIMATE_SCRATCH_LIGHT_DAMAGE,
            knockback: 0.7,
            vertical_knockback: 0.0,
            guardable: false,
            impact_cue: "impact_ultimate_scratch",
            hitstop_scale: 0.82,
            shake_scale: 0.78,
            feedback_priority_bonus: 9,
        },
        AttackPayloadId::UltimateScratchHeavy => AttackPayloadDef {
            id,
            kind: AttackKind::Ultimate,
            shape_id: AttackShapeId::UltimateScratchRight,
            reaction_family: ReactionFamilyId::UltimateLockedStagger,
            damage_profile: DamageProfileId::UltimateRush,
            element: DamageElement::Strike,
            power: 13.0,
            str_scale: 0.8,
            time_ms: 100,
            damage: ULTIMATE_SCRATCH_HEAVY_DAMAGE,
            knockback: 0.9,
            vertical_knockback: 0.0,
            guardable: false,
            impact_cue: "impact_ultimate_scratch",
            hitstop_scale: 0.88,
            shake_scale: 0.86,
            feedback_priority_bonus: 10,
        },
        AttackPayloadId::UltimateBomb => AttackPayloadDef {
            id,
            kind: AttackKind::Ultimate,
            shape_id: AttackShapeId::UltimateBomb,
            reaction_family: ReactionFamilyId::UltimateBombDown,
            damage_profile: DamageProfileId::UltimateRush,
            element: DamageElement::Blast,
            power: 15.0,
            str_scale: 0.8,
            time_ms: 200,
            damage: ULTIMATE_BOMB_DAMAGE,
            knockback: 8.8,
            vertical_knockback: 3.8,
            guardable: false,
            impact_cue: "impact_ultimate_bomb",
            hitstop_scale: 1.42,
            shake_scale: 1.5,
            feedback_priority_bonus: 16,
        },
        AttackPayloadId::DogUltimateCatch => AttackPayloadDef {
            id,
            kind: AttackKind::Ultimate,
            shape_id: AttackShapeId::UltimateCatch,
            reaction_family: ReactionFamilyId::HeavyStandingStagger,
            damage_profile: DamageProfileId::UltimateRush,
            element: DamageElement::Strike,
            power: 11.0,
            str_scale: 0.8,
            time_ms: 100,
            damage: ULTIMATE_CATCH_DAMAGE * 1.06,
            knockback: 1.5,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_dog_ultimate_catch",
            hitstop_scale: 1.16,
            shake_scale: 1.08,
            feedback_priority_bonus: 8,
        },
        AttackPayloadId::DogUltimateScratchLight => AttackPayloadDef {
            id,
            kind: AttackKind::Ultimate,
            shape_id: AttackShapeId::UltimateScratchLeft,
            reaction_family: ReactionFamilyId::UltimateLockedStagger,
            damage_profile: DamageProfileId::UltimateRush,
            element: DamageElement::Strike,
            power: 11.0,
            str_scale: 0.8,
            time_ms: 100,
            damage: ULTIMATE_SCRATCH_LIGHT_DAMAGE * 1.08,
            knockback: 0.75,
            vertical_knockback: 0.0,
            guardable: false,
            impact_cue: "impact_dog_ultimate_scratch",
            hitstop_scale: 0.9,
            shake_scale: 0.86,
            feedback_priority_bonus: 9,
        },
        AttackPayloadId::DogUltimateScratchHeavy => AttackPayloadDef {
            id,
            kind: AttackKind::Ultimate,
            shape_id: AttackShapeId::UltimateScratchRight,
            reaction_family: ReactionFamilyId::UltimateLockedStagger,
            damage_profile: DamageProfileId::UltimateRush,
            element: DamageElement::Strike,
            power: 13.0,
            str_scale: 0.8,
            time_ms: 100,
            damage: ULTIMATE_SCRATCH_HEAVY_DAMAGE * 1.1,
            knockback: 0.95,
            vertical_knockback: 0.0,
            guardable: false,
            impact_cue: "impact_dog_ultimate_scratch",
            hitstop_scale: 0.96,
            shake_scale: 0.94,
            feedback_priority_bonus: 10,
        },
        AttackPayloadId::DogUltimateBomb => AttackPayloadDef {
            id,
            kind: AttackKind::Ultimate,
            shape_id: AttackShapeId::UltimateBomb,
            reaction_family: ReactionFamilyId::UltimateBombDown,
            damage_profile: DamageProfileId::UltimateRush,
            element: DamageElement::Blast,
            power: 15.0,
            str_scale: 0.8,
            time_ms: 200,
            damage: ULTIMATE_BOMB_DAMAGE * 1.12,
            knockback: 9.2,
            vertical_knockback: 4.0,
            guardable: false,
            impact_cue: "impact_dog_ultimate_bomb",
            hitstop_scale: 1.48,
            shake_scale: 1.54,
            feedback_priority_bonus: 16,
        },
        AttackPayloadId::FoxUltimateCatch => AttackPayloadDef {
            id,
            kind: AttackKind::Ultimate,
            shape_id: AttackShapeId::UltimateCatch,
            reaction_family: ReactionFamilyId::ShortStandingStagger,
            damage_profile: DamageProfileId::UltimateRush,
            element: DamageElement::Wind,
            power: 9.0,
            str_scale: 0.7,
            time_ms: 80,
            damage: ULTIMATE_CATCH_DAMAGE * 0.9,
            knockback: 1.1,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_fox_ultimate_catch",
            hitstop_scale: 0.96,
            shake_scale: 0.88,
            feedback_priority_bonus: 8,
        },
        AttackPayloadId::FoxUltimateScratchLight => AttackPayloadDef {
            id,
            kind: AttackKind::Ultimate,
            shape_id: AttackShapeId::UltimateScratchLeft,
            reaction_family: ReactionFamilyId::UltimateLockedStagger,
            damage_profile: DamageProfileId::UltimateRush,
            element: DamageElement::Wind,
            power: 9.0,
            str_scale: 0.7,
            time_ms: 80,
            damage: ULTIMATE_SCRATCH_LIGHT_DAMAGE * 0.86,
            knockback: 0.55,
            vertical_knockback: 0.0,
            guardable: false,
            impact_cue: "impact_fox_ultimate_scratch",
            hitstop_scale: 0.72,
            shake_scale: 0.66,
            feedback_priority_bonus: 9,
        },
        AttackPayloadId::FoxUltimateScratchHeavy => AttackPayloadDef {
            id,
            kind: AttackKind::Ultimate,
            shape_id: AttackShapeId::UltimateScratchRight,
            reaction_family: ReactionFamilyId::UltimateLockedStagger,
            damage_profile: DamageProfileId::UltimateRush,
            element: DamageElement::Wind,
            power: 11.0,
            str_scale: 0.7,
            time_ms: 80,
            damage: ULTIMATE_SCRATCH_HEAVY_DAMAGE * 0.9,
            knockback: 0.72,
            vertical_knockback: 0.0,
            guardable: false,
            impact_cue: "impact_fox_ultimate_scratch",
            hitstop_scale: 0.76,
            shake_scale: 0.72,
            feedback_priority_bonus: 10,
        },
        AttackPayloadId::FoxUltimateBomb => AttackPayloadDef {
            id,
            kind: AttackKind::Ultimate,
            shape_id: AttackShapeId::UltimateBomb,
            reaction_family: ReactionFamilyId::UltimateBombDown,
            damage_profile: DamageProfileId::UltimateRush,
            element: DamageElement::Blast,
            power: 13.0,
            str_scale: 0.7,
            time_ms: 160,
            damage: ULTIMATE_BOMB_DAMAGE * 0.92,
            knockback: 8.0,
            vertical_knockback: 3.2,
            guardable: false,
            impact_cue: "impact_fox_ultimate_bomb",
            hitstop_scale: 1.24,
            shake_scale: 1.24,
            feedback_priority_bonus: 15,
        },
        AttackPayloadId::PandaUltimateCatch => AttackPayloadDef {
            id,
            kind: AttackKind::Ultimate,
            shape_id: AttackShapeId::UltimateCatch,
            reaction_family: ReactionFamilyId::HeavyStandingStagger,
            damage_profile: DamageProfileId::UltimateRush,
            element: DamageElement::Earth,
            power: 13.0,
            str_scale: 0.8,
            time_ms: 120,
            damage: ULTIMATE_CATCH_DAMAGE * 1.18,
            knockback: 1.9,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_panda_ultimate_catch",
            hitstop_scale: 1.28,
            shake_scale: 1.22,
            feedback_priority_bonus: 9,
        },
        AttackPayloadId::PandaUltimateScratchLight => AttackPayloadDef {
            id,
            kind: AttackKind::Ultimate,
            shape_id: AttackShapeId::UltimateScratchLeft,
            reaction_family: ReactionFamilyId::UltimateLockedStagger,
            damage_profile: DamageProfileId::UltimateRush,
            element: DamageElement::Earth,
            power: 13.0,
            str_scale: 0.8,
            time_ms: 120,
            damage: ULTIMATE_SCRATCH_LIGHT_DAMAGE * 1.2,
            knockback: 0.9,
            vertical_knockback: 0.0,
            guardable: false,
            impact_cue: "impact_panda_ultimate_scratch",
            hitstop_scale: 1.04,
            shake_scale: 1.0,
            feedback_priority_bonus: 10,
        },
        AttackPayloadId::PandaUltimateScratchHeavy => AttackPayloadDef {
            id,
            kind: AttackKind::Ultimate,
            shape_id: AttackShapeId::UltimateScratchRight,
            reaction_family: ReactionFamilyId::UltimateLockedStagger,
            damage_profile: DamageProfileId::UltimateRush,
            element: DamageElement::Earth,
            power: 15.0,
            str_scale: 0.8,
            time_ms: 120,
            damage: ULTIMATE_SCRATCH_HEAVY_DAMAGE * 1.25,
            knockback: 1.1,
            vertical_knockback: 0.0,
            guardable: false,
            impact_cue: "impact_panda_ultimate_scratch",
            hitstop_scale: 1.1,
            shake_scale: 1.08,
            feedback_priority_bonus: 11,
        },
        AttackPayloadId::PandaUltimateBomb => AttackPayloadDef {
            id,
            kind: AttackKind::Ultimate,
            shape_id: AttackShapeId::UltimateBomb,
            reaction_family: ReactionFamilyId::UltimateBombDown,
            damage_profile: DamageProfileId::UltimateRush,
            element: DamageElement::Blast,
            power: 15.0,
            str_scale: 0.8,
            time_ms: 240,
            damage: ULTIMATE_BOMB_DAMAGE * 1.3,
            knockback: 10.0,
            vertical_knockback: 4.6,
            guardable: false,
            impact_cue: "impact_panda_ultimate_bomb",
            hitstop_scale: 1.65,
            shake_scale: 1.75,
            feedback_priority_bonus: 17,
        },
        AttackPayloadId::BeeUltimateCatch => AttackPayloadDef {
            id,
            kind: AttackKind::Ultimate,
            shape_id: AttackShapeId::UltimateCatch,
            reaction_family: ReactionFamilyId::ShortStandingStagger,
            damage_profile: DamageProfileId::UltimateRush,
            element: DamageElement::Wind,
            power: 10.0,
            str_scale: 0.75,
            time_ms: 90,
            damage: ULTIMATE_CATCH_DAMAGE * 0.95,
            knockback: 1.2,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_bee_ultimate_catch",
            hitstop_scale: 1.0,
            shake_scale: 0.94,
            feedback_priority_bonus: 8,
        },
        AttackPayloadId::BeeUltimateScratchLight => AttackPayloadDef {
            id,
            kind: AttackKind::Ultimate,
            shape_id: AttackShapeId::UltimateScratchLeft,
            reaction_family: ReactionFamilyId::UltimateLockedStagger,
            damage_profile: DamageProfileId::UltimateRush,
            element: DamageElement::Wind,
            power: 10.0,
            str_scale: 0.75,
            time_ms: 90,
            damage: ULTIMATE_SCRATCH_LIGHT_DAMAGE * 0.92,
            knockback: 0.58,
            vertical_knockback: 0.0,
            guardable: false,
            impact_cue: "impact_bee_ultimate_scratch",
            hitstop_scale: 0.78,
            shake_scale: 0.72,
            feedback_priority_bonus: 9,
        },
        AttackPayloadId::BeeUltimateScratchHeavy => AttackPayloadDef {
            id,
            kind: AttackKind::Ultimate,
            shape_id: AttackShapeId::UltimateScratchRight,
            reaction_family: ReactionFamilyId::UltimateLockedStagger,
            damage_profile: DamageProfileId::UltimateRush,
            element: DamageElement::Wind,
            power: 11.0,
            str_scale: 0.75,
            time_ms: 90,
            damage: ULTIMATE_SCRATCH_HEAVY_DAMAGE * 0.96,
            knockback: 0.78,
            vertical_knockback: 0.0,
            guardable: false,
            impact_cue: "impact_bee_ultimate_scratch",
            hitstop_scale: 0.84,
            shake_scale: 0.8,
            feedback_priority_bonus: 10,
        },
        AttackPayloadId::BeeUltimateBomb => AttackPayloadDef {
            id,
            kind: AttackKind::Ultimate,
            shape_id: AttackShapeId::UltimateBomb,
            reaction_family: ReactionFamilyId::UltimateBombDown,
            damage_profile: DamageProfileId::UltimateRush,
            element: DamageElement::Blast,
            power: 13.0,
            str_scale: 0.75,
            time_ms: 180,
            damage: ULTIMATE_BOMB_DAMAGE * 0.98,
            knockback: 8.4,
            vertical_knockback: 3.6,
            guardable: false,
            impact_cue: "impact_bee_ultimate_bomb",
            hitstop_scale: 1.32,
            shake_scale: 1.36,
            feedback_priority_bonus: 15,
        },
        AttackPayloadId::BeeUltimateSwarmTick => AttackPayloadDef {
            id,
            kind: AttackKind::Ultimate,
            shape_id: AttackShapeId::HazardField,
            reaction_family: ReactionFamilyId::ShortStandingStagger,
            damage_profile: DamageProfileId::Direct,
            element: DamageElement::Wind,
            power: 6.5,
            str_scale: 0.45,
            time_ms: 90,
            damage: 10.0,
            knockback: 0.9,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_bee_ultimate_swarm",
            hitstop_scale: 0.46,
            shake_scale: 0.38,
            feedback_priority_bonus: 4,
        },
        AttackPayloadId::PenguinUltimateCatch => AttackPayloadDef {
            id,
            kind: AttackKind::Ultimate,
            shape_id: AttackShapeId::UltimateCatch,
            reaction_family: ReactionFamilyId::HeavyStandingStagger,
            damage_profile: DamageProfileId::UltimateRush,
            element: DamageElement::Strike,
            power: 12.0,
            str_scale: 0.8,
            time_ms: 120,
            damage: ULTIMATE_CATCH_DAMAGE,
            knockback: 1.6,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_penguin_ultimate_catch",
            hitstop_scale: 1.18,
            shake_scale: 1.12,
            feedback_priority_bonus: 8,
        },
        AttackPayloadId::PenguinUltimateScratchLight => AttackPayloadDef {
            id,
            kind: AttackKind::Ultimate,
            shape_id: AttackShapeId::UltimateScratchLeft,
            reaction_family: ReactionFamilyId::UltimateLockedStagger,
            damage_profile: DamageProfileId::UltimateRush,
            element: DamageElement::Wind,
            power: 11.0,
            str_scale: 0.75,
            time_ms: 100,
            damage: ULTIMATE_SCRATCH_LIGHT_DAMAGE,
            knockback: 0.7,
            vertical_knockback: 0.0,
            guardable: false,
            impact_cue: "impact_penguin_ultimate_flurry",
            hitstop_scale: 0.88,
            shake_scale: 0.82,
            feedback_priority_bonus: 9,
        },
        AttackPayloadId::PenguinUltimateScratchHeavy => AttackPayloadDef {
            id,
            kind: AttackKind::Ultimate,
            shape_id: AttackShapeId::UltimateScratchRight,
            reaction_family: ReactionFamilyId::UltimateLockedStagger,
            damage_profile: DamageProfileId::UltimateRush,
            element: DamageElement::Strike,
            power: 13.0,
            str_scale: 0.8,
            time_ms: 110,
            damage: ULTIMATE_SCRATCH_HEAVY_DAMAGE * 1.08,
            knockback: 0.98,
            vertical_knockback: 0.0,
            guardable: false,
            impact_cue: "impact_penguin_ultimate_bonk",
            hitstop_scale: 1.0,
            shake_scale: 0.98,
            feedback_priority_bonus: 10,
        },
        AttackPayloadId::PenguinUltimateBomb => AttackPayloadDef {
            id,
            kind: AttackKind::Ultimate,
            shape_id: AttackShapeId::UltimateBomb,
            reaction_family: ReactionFamilyId::UltimateBombDown,
            damage_profile: DamageProfileId::UltimateRush,
            element: DamageElement::Blast,
            power: 15.0,
            str_scale: 0.8,
            time_ms: 220,
            damage: ULTIMATE_BOMB_DAMAGE * 1.12,
            knockback: 9.1,
            vertical_knockback: 4.0,
            guardable: false,
            impact_cue: "impact_penguin_snowflake_burst",
            hitstop_scale: 1.5,
            shake_scale: 1.58,
            feedback_priority_bonus: 16,
        },
        AttackPayloadId::PenguinUltimateSlopeCrash => AttackPayloadDef {
            id,
            kind: AttackKind::Ultimate,
            shape_id: AttackShapeId::PenguinUltimateSlopeBody,
            reaction_family: ReactionFamilyId::AirFishKnockdown,
            damage_profile: DamageProfileId::UltimateRush,
            element: DamageElement::Launch,
            power: 15.0,
            str_scale: 0.8,
            time_ms: 980,
            damage: ULTIMATE_BOMB_DAMAGE * 1.45,
            knockback: PENGUIN_SLOPE_TOTAL_FORWARD * 1.7,
            vertical_knockback: 6.2,
            guardable: true,
            impact_cue: "impact_penguin_ultimate_slope_crash",
            hitstop_scale: PENGUIN_SLOPE_ULTIMATE_HITSTOP_SCALE,
            shake_scale: 1.32,
            feedback_priority_bonus: 14,
        },
        AttackPayloadId::PigUltimateCatch => AttackPayloadDef {
            id,
            kind: AttackKind::Ultimate,
            shape_id: AttackShapeId::PigUltimateGrab,
            reaction_family: ReactionFamilyId::HeavyStandingStagger,
            damage_profile: DamageProfileId::UltimateRush,
            element: DamageElement::Earth,
            power: 13.0,
            str_scale: 0.8,
            time_ms: 190,
            damage: ULTIMATE_CATCH_DAMAGE * 0.8,
            knockback: 1.0,
            vertical_knockback: 0.0,
            guardable: false,
            impact_cue: "impact_pig_ultimate_catch",
            hitstop_scale: 1.35,
            shake_scale: 1.25,
            feedback_priority_bonus: 10,
        },
        AttackPayloadId::PigUltimateScratchLight => AttackPayloadDef {
            id,
            kind: AttackKind::Ultimate,
            shape_id: AttackShapeId::PigMeatSlam,
            reaction_family: ReactionFamilyId::UltimateLockedStagger,
            damage_profile: DamageProfileId::UltimateRush,
            element: DamageElement::Earth,
            power: 13.0,
            str_scale: 0.8,
            time_ms: 150,
            damage: ULTIMATE_SCRATCH_LIGHT_DAMAGE * 1.28,
            knockback: 0.9,
            vertical_knockback: 0.0,
            guardable: false,
            impact_cue: "impact_pig_ultimate_scratch",
            hitstop_scale: 1.16,
            shake_scale: 1.15,
            feedback_priority_bonus: 10,
        },
        AttackPayloadId::PigUltimateScratchHeavy => AttackPayloadDef {
            id,
            kind: AttackKind::Ultimate,
            shape_id: AttackShapeId::PigHalfCircleSwing,
            reaction_family: ReactionFamilyId::UltimateLockedStagger,
            damage_profile: DamageProfileId::UltimateRush,
            element: DamageElement::Earth,
            power: 15.0,
            str_scale: 0.8,
            time_ms: 170,
            damage: ULTIMATE_SCRATCH_HEAVY_DAMAGE * 1.36,
            knockback: 1.25,
            vertical_knockback: 0.0,
            guardable: false,
            impact_cue: "impact_pig_ultimate_scratch_heavy",
            hitstop_scale: 1.22,
            shake_scale: 1.2,
            feedback_priority_bonus: 12,
        },
        AttackPayloadId::PigUltimateBomb => AttackPayloadDef {
            id,
            kind: AttackKind::Ultimate,
            shape_id: AttackShapeId::UltimateBomb,
            reaction_family: ReactionFamilyId::UltimateBombDown,
            damage_profile: DamageProfileId::UltimateRush,
            element: DamageElement::Blast,
            power: 15.0,
            str_scale: 0.8,
            time_ms: 270,
            damage: ULTIMATE_BOMB_DAMAGE * 1.45,
            knockback: 10.5,
            vertical_knockback: 5.0,
            guardable: false,
            impact_cue: "impact_pig_ultimate_bomb",
            hitstop_scale: 1.78,
            shake_scale: 1.9,
            feedback_priority_bonus: 19,
        },
        AttackPayloadId::ComboFinisherLift => AttackPayloadDef {
            id,
            kind: AttackKind::ComboFinisher,
            shape_id: AttackShapeId::CompactThrust,
            reaction_family: ReactionFamilyId::CounterPop,
            damage_profile: DamageProfileId::GroundBounce,
            element: DamageElement::Earth,
            power: 11.0,
            str_scale: 0.8,
            time_ms: 110,
            damage: COMBO_FINISHER_DAMAGE * 0.58,
            knockback: COMBO_FINISHER_KNOCKBACK * 0.7,
            vertical_knockback: 2.6,
            guardable: true,
            impact_cue: "impact_combo_finisher_lift",
            hitstop_scale: 1.14,
            shake_scale: 1.1,
            feedback_priority_bonus: 5,
        },
        AttackPayloadId::ComboFinisher => AttackPayloadDef {
            id,
            kind: AttackKind::ComboFinisher,
            shape_id: AttackShapeId::CatBodySkid,
            reaction_family: ReactionFamilyId::GroundBounceDown,
            damage_profile: DamageProfileId::GroundBounce,
            element: DamageElement::Earth,
            power: 13.0,
            str_scale: 0.8,
            time_ms: 100,
            damage: COMBO_FINISHER_DAMAGE,
            knockback: COMBO_FINISHER_KNOCKBACK,
            vertical_knockback: 4.2,
            guardable: true,
            impact_cue: "impact_combo_finisher",
            hitstop_scale: 1.22,
            shake_scale: 1.2,
            feedback_priority_bonus: 6,
        },
        AttackPayloadId::DashComboFinisher => AttackPayloadDef {
            id,
            kind: AttackKind::ComboFinisher,
            shape_id: AttackShapeId::CatBodySkid,
            reaction_family: ReactionFamilyId::GroundBounceDown,
            damage_profile: DamageProfileId::GroundBounce,
            element: DamageElement::Earth,
            power: 13.0,
            str_scale: 0.8,
            time_ms: 430,
            damage: COMBO_FINISHER_DAMAGE,
            knockback: COMBO_FINISHER_KNOCKBACK,
            vertical_knockback: 4.2,
            guardable: true,
            impact_cue: "impact_combo_finisher",
            hitstop_scale: 1.22,
            shake_scale: 1.2,
            feedback_priority_bonus: 6,
        },
        AttackPayloadId::PigSnoutShove => AttackPayloadDef {
            id,
            kind: AttackKind::Light1,
            shape_id: AttackShapeId::PigBodyShove,
            reaction_family: ReactionFamilyId::HeavyStandingStagger,
            damage_profile: DamageProfileId::BasicStrike,
            element: DamageElement::Strike,
            power: 13.0,
            str_scale: 0.8,
            time_ms: 160,
            damage: LIGHT_DAMAGE * 1.24,
            knockback: LIGHT_KNOCKBACK * 0.16,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_pig_snout_shove",
            hitstop_scale: 1.18,
            shake_scale: 1.16,
            feedback_priority_bonus: 5,
        },
        AttackPayloadId::PigBellyBump => AttackPayloadDef {
            id,
            kind: AttackKind::Light2,
            shape_id: AttackShapeId::PigBellyCrash,
            reaction_family: ReactionFamilyId::HeavyStandingStagger,
            damage_profile: DamageProfileId::FollowupStrike,
            element: DamageElement::Earth,
            power: 15.0,
            str_scale: 0.8,
            time_ms: 180,
            damage: LIGHT2_DAMAGE * 1.28,
            knockback: LIGHT2_KNOCKBACK * 0.2,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_pig_belly_bump",
            hitstop_scale: 1.24,
            shake_scale: 1.28,
            feedback_priority_bonus: 6,
        },
        AttackPayloadId::PigHamSlam => AttackPayloadDef {
            id,
            kind: AttackKind::ComboFinisher,
            shape_id: AttackShapeId::PigMeatSlam,
            reaction_family: ReactionFamilyId::GroundedDownGetup,
            damage_profile: DamageProfileId::GroundBounce,
            element: DamageElement::Earth,
            power: 15.0,
            str_scale: 0.8,
            time_ms: 430,
            damage: COMBO_FINISHER_DAMAGE * 1.42,
            knockback: 0.0,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_pig_ham_slam",
            hitstop_scale: 1.55,
            shake_scale: 1.62,
            feedback_priority_bonus: 12,
        },
        AttackPayloadId::PigHamSwingTap => AttackPayloadDef {
            id,
            kind: AttackKind::Heavy,
            shape_id: AttackShapeId::PigHalfCircleSwing,
            reaction_family: ReactionFamilyId::HeavyStandingStagger,
            damage_profile: DamageProfileId::BasicStrike,
            element: DamageElement::Strike,
            power: 9.0,
            str_scale: 0.8,
            time_ms: 150,
            damage: HEAVY_DAMAGE * 0.55,
            knockback: HEAVY_KNOCKBACK * 0.32,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_pig_ham_swing_tap",
            hitstop_scale: 1.18,
            shake_scale: 1.18,
            feedback_priority_bonus: 6,
        },
        AttackPayloadId::PigHamSwingPartial => AttackPayloadDef {
            id,
            kind: AttackKind::Heavy,
            shape_id: AttackShapeId::PigHalfCircleSwing,
            reaction_family: ReactionFamilyId::SlidingKnockdown,
            damage_profile: DamageProfileId::GroundBounce,
            element: DamageElement::Earth,
            power: 13.0,
            str_scale: 0.8,
            time_ms: 180,
            damage: HEAVY_DAMAGE * 0.95,
            knockback: HEAVY_KNOCKBACK * 0.95,
            vertical_knockback: 0.7,
            guardable: true,
            impact_cue: "impact_pig_ham_swing_partial",
            hitstop_scale: 1.32,
            shake_scale: 1.38,
            feedback_priority_bonus: 9,
        },
        AttackPayloadId::PigHamSwingFull => AttackPayloadDef {
            id,
            kind: AttackKind::Heavy,
            shape_id: AttackShapeId::PigHalfCircleSwing,
            reaction_family: ReactionFamilyId::GroundBounceDown,
            damage_profile: DamageProfileId::GroundBounce,
            element: DamageElement::Earth,
            power: 15.0,
            str_scale: 0.8,
            time_ms: 220,
            damage: HEAVY_DAMAGE * 1.35,
            knockback: HEAVY_KNOCKBACK * 1.7,
            vertical_knockback: 4.4,
            guardable: true,
            impact_cue: "impact_pig_ham_swing_full",
            hitstop_scale: 1.62,
            shake_scale: 1.78,
            feedback_priority_bonus: 14,
        },
        AttackPayloadId::PigRollingPinStep => AttackPayloadDef {
            id,
            kind: AttackKind::Heavy,
            shape_id: AttackShapeId::PigRollingPinLine,
            reaction_family: ReactionFamilyId::HeavyStandingStagger,
            damage_profile: DamageProfileId::DashBody,
            element: DamageElement::Strike,
            power: 13.0,
            str_scale: 0.8,
            time_ms: 180,
            damage: HEAVY_DAMAGE * 0.72,
            knockback: HEAVY_KNOCKBACK * 0.24,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_pig_rolling_pin",
            hitstop_scale: 1.28,
            shake_scale: 1.3,
            feedback_priority_bonus: 7,
        },
        AttackPayloadId::PigHamLauncher => AttackPayloadDef {
            id,
            kind: AttackKind::Heavy,
            shape_id: AttackShapeId::DelayedRiser,
            reaction_family: ReactionFamilyId::LauncherDown,
            damage_profile: DamageProfileId::LauncherCommit,
            element: DamageElement::Launch,
            power: 15.0,
            str_scale: 0.8,
            time_ms: 190,
            damage: HEAVY_DAMAGE * 1.26,
            knockback: HEAVY_KNOCKBACK * 0.72,
            vertical_knockback: 8.2,
            guardable: true,
            impact_cue: "impact_pig_ham_launcher",
            hitstop_scale: 1.52,
            shake_scale: 1.58,
            feedback_priority_bonus: 12,
        },
        AttackPayloadId::PigAirMeatSlam => AttackPayloadDef {
            id,
            kind: AttackKind::Jump,
            shape_id: AttackShapeId::PigAirMeatSlam,
            reaction_family: ReactionFamilyId::LauncherDown,
            damage_profile: DamageProfileId::LauncherCommit,
            element: DamageElement::Launch,
            power: 15.0,
            str_scale: 0.8,
            time_ms: 360,
            damage: JUMP_HEAVY_DAMAGE * 1.2,
            knockback: JUMP_HEAVY_KNOCKBACK * 0.42,
            vertical_knockback: 10.56,
            guardable: true,
            impact_cue: "impact_pig_air_meat_slam",
            hitstop_scale: 1.5,
            shake_scale: 1.58,
            feedback_priority_bonus: 13,
        },
        AttackPayloadId::DogBite1 => AttackPayloadDef {
            id,
            kind: AttackKind::Light1,
            shape_id: AttackShapeId::CompactThrust,
            reaction_family: ReactionFamilyId::HeavyStandingStagger,
            damage_profile: DamageProfileId::BasicStrike,
            element: DamageElement::Strike,
            power: 10.0,
            str_scale: 0.8,
            time_ms: 120,
            damage: LIGHT_DAMAGE * 1.08,
            knockback: LIGHT_KNOCKBACK * 0.18,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_dog_bite_1",
            hitstop_scale: 1.05,
            shake_scale: 1.04,
            feedback_priority_bonus: 3,
        },
        AttackPayloadId::DogBite2 => AttackPayloadDef {
            id,
            kind: AttackKind::Light2,
            shape_id: AttackShapeId::CompactThrust,
            reaction_family: ReactionFamilyId::HeavyStandingStagger,
            damage_profile: DamageProfileId::FollowupStrike,
            element: DamageElement::Strike,
            power: 11.0,
            str_scale: 0.8,
            time_ms: 120,
            damage: LIGHT2_DAMAGE * 1.08,
            knockback: LIGHT2_KNOCKBACK * 0.2,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_dog_bite_2",
            hitstop_scale: 1.08,
            shake_scale: 1.08,
            feedback_priority_bonus: 4,
        },
        AttackPayloadId::DogBodyPounce => AttackPayloadDef {
            id,
            kind: AttackKind::ComboFinisher,
            shape_id: AttackShapeId::GroundSkid,
            reaction_family: ReactionFamilyId::GroundBounceDown,
            damage_profile: DamageProfileId::GroundBounce,
            element: DamageElement::Earth,
            power: 15.0,
            str_scale: 0.8,
            time_ms: 120,
            damage: COMBO_FINISHER_DAMAGE * 1.14,
            knockback: COMBO_FINISHER_KNOCKBACK * 1.08,
            vertical_knockback: 4.6,
            guardable: true,
            impact_cue: "impact_dog_pounce",
            hitstop_scale: 1.32,
            shake_scale: 1.32,
            feedback_priority_bonus: 8,
        },
        AttackPayloadId::DogShoulderStep => AttackPayloadDef {
            id,
            kind: AttackKind::Heavy,
            shape_id: AttackShapeId::ShoulderLine,
            reaction_family: ReactionFamilyId::HeavyStandingStagger,
            damage_profile: DamageProfileId::DashBody,
            element: DamageElement::Strike,
            power: 13.0,
            str_scale: 0.8,
            time_ms: 130,
            damage: HEAVY_DAMAGE * 0.5,
            knockback: HEAVY_KNOCKBACK * 0.22,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_dog_shoulder",
            hitstop_scale: 1.16,
            shake_scale: 1.16,
            feedback_priority_bonus: 5,
        },
        AttackPayloadId::DogLaunchBite => AttackPayloadDef {
            id,
            kind: AttackKind::Heavy,
            shape_id: AttackShapeId::DelayedRiser,
            reaction_family: ReactionFamilyId::LauncherDown,
            damage_profile: DamageProfileId::LauncherCommit,
            element: DamageElement::Launch,
            power: 15.0,
            str_scale: 0.8,
            time_ms: 120,
            damage: HEAVY_DAMAGE * 1.08,
            knockback: HEAVY_KNOCKBACK * 0.78,
            vertical_knockback: 7.0,
            guardable: true,
            impact_cue: "impact_dog_launch_bite",
            hitstop_scale: 1.34,
            shake_scale: 1.34,
            feedback_priority_bonus: 9,
        },
        AttackPayloadId::FoxSwipe1 => AttackPayloadDef {
            id,
            kind: AttackKind::Light1,
            shape_id: AttackShapeId::HookSweep,
            reaction_family: ReactionFamilyId::ShortStandingStagger,
            damage_profile: DamageProfileId::BasicStrike,
            element: DamageElement::Wind,
            power: 9.0,
            str_scale: 0.7,
            time_ms: 80,
            damage: LIGHT_DAMAGE * 0.82,
            knockback: LIGHT_KNOCKBACK * 0.12,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_fox_swipe_1",
            hitstop_scale: 0.78,
            shake_scale: 0.72,
            feedback_priority_bonus: 2,
        },
        AttackPayloadId::FoxSwipe2 => AttackPayloadDef {
            id,
            kind: AttackKind::Light2,
            shape_id: AttackShapeId::SweepingArcWide,
            reaction_family: ReactionFamilyId::ShortStandingStagger,
            damage_profile: DamageProfileId::FollowupStrike,
            element: DamageElement::Wind,
            power: 9.0,
            str_scale: 0.7,
            time_ms: 80,
            damage: LIGHT2_DAMAGE * 0.82,
            knockback: LIGHT2_KNOCKBACK * 0.14,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_fox_swipe_2",
            hitstop_scale: 0.8,
            shake_scale: 0.76,
            feedback_priority_bonus: 3,
        },
        AttackPayloadId::FoxTailSweep => AttackPayloadDef {
            id,
            kind: AttackKind::ComboFinisher,
            shape_id: AttackShapeId::SweepingArcWide,
            reaction_family: ReactionFamilyId::SlidingKnockdown,
            damage_profile: DamageProfileId::GroundBounce,
            element: DamageElement::Wind,
            power: 11.0,
            str_scale: 0.7,
            time_ms: 90,
            damage: COMBO_FINISHER_DAMAGE * 0.88,
            knockback: COMBO_FINISHER_KNOCKBACK * 0.92,
            vertical_knockback: 3.4,
            guardable: true,
            impact_cue: "impact_fox_tail_sweep",
            hitstop_scale: 1.08,
            shake_scale: 1.0,
            feedback_priority_bonus: 6,
        },
        AttackPayloadId::FoxSkitterStep => AttackPayloadDef {
            id,
            kind: AttackKind::Heavy,
            shape_id: AttackShapeId::ShoulderLine,
            reaction_family: ReactionFamilyId::ShortStandingStagger,
            damage_profile: DamageProfileId::DashBody,
            element: DamageElement::Wind,
            power: 10.0,
            str_scale: 0.7,
            time_ms: 80,
            damage: HEAVY_DAMAGE * 0.38,
            knockback: HEAVY_KNOCKBACK * 0.12,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_fox_skitter",
            hitstop_scale: 0.86,
            shake_scale: 0.82,
            feedback_priority_bonus: 4,
        },
        AttackPayloadId::FoxFlipLaunch => AttackPayloadDef {
            id,
            kind: AttackKind::Heavy,
            shape_id: AttackShapeId::RisingColumn,
            reaction_family: ReactionFamilyId::LauncherDown,
            damage_profile: DamageProfileId::LauncherCommit,
            element: DamageElement::Launch,
            power: 13.0,
            str_scale: 0.7,
            time_ms: 90,
            damage: HEAVY_DAMAGE * 0.82,
            knockback: HEAVY_KNOCKBACK * 0.6,
            vertical_knockback: 6.8,
            guardable: true,
            impact_cue: "impact_fox_flip_launch",
            hitstop_scale: 1.14,
            shake_scale: 1.08,
            feedback_priority_bonus: 7,
        },
        AttackPayloadId::PandaPalm1 => AttackPayloadDef {
            id,
            kind: AttackKind::Light1,
            shape_id: AttackShapeId::CompactSlashTight,
            reaction_family: ReactionFamilyId::HeavyStandingStagger,
            damage_profile: DamageProfileId::BasicStrike,
            element: DamageElement::Earth,
            power: 11.0,
            str_scale: 0.8,
            time_ms: 140,
            damage: LIGHT_DAMAGE * 1.18,
            knockback: LIGHT_KNOCKBACK * 0.24,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_panda_palm_1",
            hitstop_scale: 1.18,
            shake_scale: 1.18,
            feedback_priority_bonus: 4,
        },
        AttackPayloadId::PandaPalm2 => AttackPayloadDef {
            id,
            kind: AttackKind::Light2,
            shape_id: AttackShapeId::BodyRoll,
            reaction_family: ReactionFamilyId::HeavyStandingStagger,
            damage_profile: DamageProfileId::FollowupStrike,
            element: DamageElement::Earth,
            power: 13.0,
            str_scale: 0.8,
            time_ms: 140,
            damage: LIGHT2_DAMAGE * 1.2,
            knockback: LIGHT2_KNOCKBACK * 0.28,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_panda_palm_2",
            hitstop_scale: 1.22,
            shake_scale: 1.24,
            feedback_priority_bonus: 5,
        },
        AttackPayloadId::PandaBodyDrop => AttackPayloadDef {
            id,
            kind: AttackKind::ComboFinisher,
            shape_id: AttackShapeId::GroundSkid,
            reaction_family: ReactionFamilyId::GroundBounceDown,
            damage_profile: DamageProfileId::GroundBounce,
            element: DamageElement::Earth,
            power: 15.0,
            str_scale: 0.8,
            time_ms: 160,
            damage: COMBO_FINISHER_DAMAGE * 1.3,
            knockback: COMBO_FINISHER_KNOCKBACK * 1.18,
            vertical_knockback: 5.2,
            guardable: true,
            impact_cue: "impact_panda_body_drop",
            hitstop_scale: 1.42,
            shake_scale: 1.5,
            feedback_priority_bonus: 10,
        },
        AttackPayloadId::PandaWeightShift => AttackPayloadDef {
            id,
            kind: AttackKind::Heavy,
            shape_id: AttackShapeId::ShoulderLine,
            reaction_family: ReactionFamilyId::HeavyStandingStagger,
            damage_profile: DamageProfileId::DashBody,
            element: DamageElement::Earth,
            power: 15.0,
            str_scale: 0.8,
            time_ms: 150,
            damage: HEAVY_DAMAGE * 0.58,
            knockback: HEAVY_KNOCKBACK * 0.3,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_panda_weight_shift",
            hitstop_scale: 1.24,
            shake_scale: 1.28,
            feedback_priority_bonus: 6,
        },
        AttackPayloadId::PandaRisingScoop => AttackPayloadDef {
            id,
            kind: AttackKind::Heavy,
            shape_id: AttackShapeId::DelayedRiser,
            reaction_family: ReactionFamilyId::LauncherDown,
            damage_profile: DamageProfileId::LauncherCommit,
            element: DamageElement::Launch,
            power: 15.0,
            str_scale: 0.8,
            time_ms: 160,
            damage: HEAVY_DAMAGE * 1.18,
            knockback: HEAVY_KNOCKBACK * 0.82,
            vertical_knockback: 7.8,
            guardable: true,
            impact_cue: "impact_panda_scoop",
            hitstop_scale: 1.42,
            shake_scale: 1.46,
            feedback_priority_bonus: 10,
        },
        AttackPayloadId::BeeNeedleTap => AttackPayloadDef {
            id,
            kind: AttackKind::Light1,
            shape_id: AttackShapeId::CompactSlashLead,
            reaction_family: ReactionFamilyId::ShortStandingStagger,
            damage_profile: DamageProfileId::BasicStrike,
            element: DamageElement::Wind,
            power: 9.0,
            str_scale: 0.75,
            time_ms: 90,
            damage: LIGHT_DAMAGE * 0.9,
            knockback: LIGHT_KNOCKBACK * 0.28,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_bee_needle_tap",
            hitstop_scale: 0.9,
            shake_scale: 0.82,
            feedback_priority_bonus: 2,
        },
        AttackPayloadId::BeeCrossSting => AttackPayloadDef {
            id,
            kind: AttackKind::Light2,
            shape_id: AttackShapeId::CompactSlashFollow,
            reaction_family: ReactionFamilyId::ShortStandingStagger,
            damage_profile: DamageProfileId::FollowupStrike,
            element: DamageElement::Wind,
            power: 9.0,
            str_scale: 0.75,
            time_ms: 90,
            damage: LIGHT2_DAMAGE * 0.9,
            knockback: LIGHT2_KNOCKBACK * 0.32,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_bee_cross_sting",
            hitstop_scale: 0.94,
            shake_scale: 0.88,
            feedback_priority_bonus: 3,
        },
        AttackPayloadId::BeeSpiralSting => AttackPayloadDef {
            id,
            kind: AttackKind::ComboFinisher,
            shape_id: AttackShapeId::CatBodySkid,
            reaction_family: ReactionFamilyId::GroundBounceDown,
            damage_profile: DamageProfileId::GroundBounce,
            element: DamageElement::Wind,
            power: 12.0,
            str_scale: 0.75,
            time_ms: 110,
            damage: COMBO_FINISHER_DAMAGE * 0.92,
            knockback: COMBO_FINISHER_KNOCKBACK * 0.96,
            vertical_knockback: 3.8,
            guardable: true,
            impact_cue: "impact_bee_spiral_sting",
            hitstop_scale: 1.14,
            shake_scale: 1.08,
            feedback_priority_bonus: 6,
        },
        AttackPayloadId::BeePiercingStep => AttackPayloadDef {
            id,
            kind: AttackKind::Heavy,
            shape_id: AttackShapeId::CompactThrust,
            reaction_family: ReactionFamilyId::HeavyStandingStagger,
            damage_profile: DamageProfileId::DashBody,
            element: DamageElement::Wind,
            power: 10.0,
            str_scale: 0.75,
            time_ms: 100,
            damage: HEAVY_DAMAGE * 0.42,
            knockback: HEAVY_KNOCKBACK * 0.18,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_bee_piercing_step",
            hitstop_scale: 1.0,
            shake_scale: 0.96,
            feedback_priority_bonus: 4,
        },
        AttackPayloadId::BeeHiveLauncher => AttackPayloadDef {
            id,
            kind: AttackKind::Heavy,
            shape_id: AttackShapeId::ProjectileBolt,
            reaction_family: ReactionFamilyId::HeavyStandingStagger,
            damage_profile: DamageProfileId::FollowupStrike,
            element: DamageElement::Wind,
            power: 13.0,
            str_scale: 0.75,
            time_ms: 100,
            damage: HEAVY_DAMAGE * 0.86,
            knockback: HEAVY_KNOCKBACK * 0.66,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_bee_hive_launcher",
            hitstop_scale: 1.22,
            shake_scale: 1.18,
            feedback_priority_bonus: 8,
        },
        AttackPayloadId::BeeAirSting => AttackPayloadDef {
            id,
            kind: AttackKind::Jump,
            shape_id: AttackShapeId::CompactThrust,
            reaction_family: ReactionFamilyId::ShortStandingStagger,
            damage_profile: DamageProfileId::BasicStrike,
            element: DamageElement::Wind,
            power: 12.0,
            str_scale: 0.75,
            time_ms: 300,
            damage: JUMP_ATTACK_DAMAGE * 0.92,
            knockback: JUMP_ATTACK_KNOCKBACK * 0.74,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_bee_air_sting",
            hitstop_scale: 1.08,
            shake_scale: 1.02,
            feedback_priority_bonus: 6,
        },
        AttackPayloadId::BeeHiveDive => AttackPayloadDef {
            id,
            kind: AttackKind::Heavy,
            shape_id: AttackShapeId::FallingSpikeArc,
            reaction_family: ReactionFamilyId::GroundBounceDown,
            damage_profile: DamageProfileId::AerialSpike,
            element: DamageElement::Wind,
            power: 13.0,
            str_scale: 0.75,
            time_ms: (JUMP_ATTACK_MAX_ACTIVE * 1000.0) as u32,
            damage: JUMP_HEAVY_DAMAGE * 0.95,
            knockback: JUMP_HEAVY_KNOCKBACK * 0.78,
            vertical_knockback: 3.4,
            guardable: true,
            impact_cue: "impact_bee_hive_dive",
            hitstop_scale: 1.18,
            shake_scale: 1.16,
            feedback_priority_bonus: 8,
        },
        AttackPayloadId::BeeWorkerSting => AttackPayloadDef {
            id,
            kind: AttackKind::Special,
            shape_id: AttackShapeId::ProjectileBolt,
            reaction_family: ReactionFamilyId::ShortStandingStagger,
            damage_profile: DamageProfileId::BasicStrike,
            element: DamageElement::Wind,
            power: 6.0,
            str_scale: 0.45,
            time_ms: 90,
            damage: 4.0,
            knockback: 3.4,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_special_projectile",
            hitstop_scale: 0.72,
            shake_scale: 0.62,
            feedback_priority_bonus: 1,
        },
        AttackPayloadId::BeeHoneyGlob => AttackPayloadDef {
            id,
            kind: AttackKind::Special,
            shape_id: AttackShapeId::ProjectileBolt,
            reaction_family: ReactionFamilyId::ShortStandingStagger,
            damage_profile: DamageProfileId::BasicStrike,
            element: DamageElement::Hazard,
            power: 9.0,
            str_scale: 0.55,
            time_ms: 120,
            damage: 6.5,
            knockback: 4.2,
            vertical_knockback: 1.7,
            guardable: true,
            impact_cue: "impact_special_projectile",
            hitstop_scale: 0.88,
            shake_scale: 0.8,
            feedback_priority_bonus: 3,
        },
        AttackPayloadId::BeeHoneyPuddle => AttackPayloadDef {
            id,
            kind: AttackKind::Special,
            shape_id: AttackShapeId::HazardField,
            reaction_family: ReactionFamilyId::ShortStandingStagger,
            damage_profile: DamageProfileId::Direct,
            element: DamageElement::Hazard,
            power: 5.0,
            str_scale: 0.35,
            time_ms: 120,
            damage: 2.8,
            knockback: 1.2,
            vertical_knockback: 0.8,
            guardable: true,
            impact_cue: "impact_special_hazard",
            hitstop_scale: 0.55,
            shake_scale: 0.46,
            feedback_priority_bonus: 1,
        },
        AttackPayloadId::BeeHomingSting => AttackPayloadDef {
            id,
            kind: AttackKind::Special,
            shape_id: AttackShapeId::ProjectileBolt,
            reaction_family: ReactionFamilyId::HeavyStandingStagger,
            damage_profile: DamageProfileId::FollowupStrike,
            element: DamageElement::Wind,
            power: 10.0,
            str_scale: 0.6,
            time_ms: 120,
            damage: 7.2,
            knockback: 5.2,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_special_projectile",
            hitstop_scale: 0.95,
            shake_scale: 0.9,
            feedback_priority_bonus: 4,
        },
        AttackPayloadId::PenguinFishTorpedo => AttackPayloadDef {
            id,
            kind: AttackKind::Special,
            shape_id: AttackShapeId::ProjectileBolt,
            reaction_family: ReactionFamilyId::ShortStandingStagger,
            damage_profile: DamageProfileId::BasicStrike,
            element: DamageElement::Wind,
            power: 8.0,
            str_scale: 0.55,
            time_ms: 105,
            damage: 5.2,
            knockback: 4.0,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_penguin_fish_torpedo",
            hitstop_scale: 0.82,
            shake_scale: 0.72,
            feedback_priority_bonus: 3,
        },
        AttackPayloadId::PenguinPopsicleBounce => AttackPayloadDef {
            id,
            kind: AttackKind::Special,
            shape_id: AttackShapeId::ProjectileBolt,
            reaction_family: ReactionFamilyId::ShortStandingStagger,
            damage_profile: DamageProfileId::BasicStrike,
            element: DamageElement::Strike,
            power: 8.0,
            str_scale: 0.55,
            time_ms: 105,
            damage: 5.8,
            knockback: 4.4,
            vertical_knockback: 1.2,
            guardable: true,
            impact_cue: "impact_penguin_popsicle_bounce",
            hitstop_scale: 0.84,
            shake_scale: 0.76,
            feedback_priority_bonus: 3,
        },
        AttackPayloadId::PenguinSledWake => AttackPayloadDef {
            id,
            kind: AttackKind::Special,
            shape_id: AttackShapeId::HazardField,
            reaction_family: ReactionFamilyId::ShortStandingStagger,
            damage_profile: DamageProfileId::Direct,
            element: DamageElement::Hazard,
            power: 5.0,
            str_scale: 0.38,
            time_ms: 120,
            damage: 2.6,
            knockback: 1.4,
            vertical_knockback: 0.6,
            guardable: true,
            impact_cue: "impact_penguin_sled_wake",
            hitstop_scale: 0.56,
            shake_scale: 0.48,
            feedback_priority_bonus: 2,
        },
        AttackPayloadId::PenguinSnowflakeShard => AttackPayloadDef {
            id,
            kind: AttackKind::Special,
            shape_id: AttackShapeId::ProjectileBolt,
            reaction_family: ReactionFamilyId::FrozenStun,
            damage_profile: DamageProfileId::Direct,
            element: DamageElement::Wind,
            power: 4.0,
            str_scale: 0.2,
            time_ms: 90,
            damage: 2.0,
            knockback: 0.0,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_penguin_snowflake_shard",
            hitstop_scale: 0.72,
            shake_scale: 0.55,
            feedback_priority_bonus: 4,
        },
        AttackPayloadId::PenguinSnowBoulder => AttackPayloadDef {
            id,
            kind: AttackKind::Special,
            shape_id: AttackShapeId::ProjectileBolt,
            reaction_family: ReactionFamilyId::HeavyStandingStagger,
            damage_profile: DamageProfileId::FollowupStrike,
            element: DamageElement::Launch,
            power: 12.0,
            str_scale: 0.68,
            time_ms: 120,
            damage: 6.8,
            knockback: 5.8,
            vertical_knockback: 1.8,
            guardable: true,
            impact_cue: "impact_penguin_snow_boulder",
            hitstop_scale: 0.96,
            shake_scale: 0.9,
            feedback_priority_bonus: 5,
        },
        AttackPayloadId::PenguinSnowmanDrop => AttackPayloadDef {
            id,
            kind: AttackKind::Special,
            shape_id: AttackShapeId::ProjectileBolt,
            reaction_family: ReactionFamilyId::FrozenStun,
            damage_profile: DamageProfileId::Direct,
            element: DamageElement::Hazard,
            power: 4.0,
            str_scale: 0.2,
            time_ms: 120,
            damage: 2.0,
            knockback: 0.0,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_special_projectile",
            hitstop_scale: 0.72,
            shake_scale: 0.55,
            feedback_priority_bonus: 2,
        },
        AttackPayloadId::PenguinBodySlamShockwave => AttackPayloadDef {
            id,
            kind: AttackKind::Special,
            shape_id: AttackShapeId::ShockwaveRing,
            reaction_family: ReactionFamilyId::GroundBounceDown,
            damage_profile: DamageProfileId::AerialSpike,
            element: DamageElement::Hazard,
            power: 15.0,
            str_scale: 0.82,
            time_ms: 155,
            damage: 8.4,
            knockback: 6.2,
            vertical_knockback: 3.8,
            guardable: true,
            impact_cue: "impact_penguin_body_slam_shockwave",
            hitstop_scale: 1.24,
            shake_scale: 1.22,
            feedback_priority_bonus: 8,
        },
        AttackPayloadId::ChickShellChip => AttackPayloadDef {
            id,
            kind: AttackKind::Special,
            shape_id: AttackShapeId::ProjectileBolt,
            reaction_family: ReactionFamilyId::ShortStandingStagger,
            damage_profile: DamageProfileId::BasicStrike,
            element: DamageElement::Strike,
            power: 5.0,
            str_scale: 0.35,
            time_ms: 75,
            damage: 2.8,
            knockback: 2.9,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_chick_shell_chip",
            hitstop_scale: 0.62,
            shake_scale: 0.46,
            feedback_priority_bonus: 1,
        },
        AttackPayloadId::ChickFriedEggDisc => AttackPayloadDef {
            id,
            kind: AttackKind::Special,
            shape_id: AttackShapeId::ProjectileBolt,
            reaction_family: ReactionFamilyId::ShortStandingStagger,
            damage_profile: DamageProfileId::BasicStrike,
            element: DamageElement::Strike,
            power: 7.0,
            str_scale: 0.48,
            time_ms: 100,
            damage: 4.8,
            knockback: 3.7,
            vertical_knockback: 0.4,
            guardable: true,
            impact_cue: "impact_chick_fried_egg_disc",
            hitstop_scale: 0.78,
            shake_scale: 0.64,
            feedback_priority_bonus: 2,
        },
        AttackPayloadId::ChickEggCupMortar => AttackPayloadDef {
            id,
            kind: AttackKind::Special,
            shape_id: AttackShapeId::ProjectileBolt,
            reaction_family: ReactionFamilyId::LightAirPop,
            damage_profile: DamageProfileId::LauncherCommit,
            element: DamageElement::Launch,
            power: 9.0,
            str_scale: 0.55,
            time_ms: 120,
            damage: 6.4,
            knockback: 4.1,
            vertical_knockback: 3.2,
            guardable: true,
            impact_cue: "impact_chick_egg_cup_mortar",
            hitstop_scale: 0.94,
            shake_scale: 0.86,
            feedback_priority_bonus: 4,
        },
        AttackPayloadId::ChickOrbitEgg => AttackPayloadDef {
            id,
            kind: AttackKind::Special,
            shape_id: AttackShapeId::ProjectileBolt,
            reaction_family: ReactionFamilyId::ShortStandingStagger,
            damage_profile: DamageProfileId::BasicStrike,
            element: DamageElement::Strike,
            power: 1.0,
            str_scale: 0.0,
            time_ms: 75,
            damage: 0.75,
            knockback: 0.6,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_special_projectile",
            hitstop_scale: 0.42,
            shake_scale: 0.28,
            feedback_priority_bonus: 1,
        },
        AttackPayloadId::ChickOrbitEggLaunch => AttackPayloadDef {
            id,
            kind: AttackKind::Special,
            shape_id: AttackShapeId::ProjectileBolt,
            reaction_family: ReactionFamilyId::SlidingKnockdown,
            damage_profile: DamageProfileId::DashBody,
            element: DamageElement::Strike,
            power: 12.0,
            str_scale: 0.62,
            time_ms: 130,
            damage: 8.0,
            knockback: 7.2,
            vertical_knockback: 0.2,
            guardable: true,
            impact_cue: "impact_special_projectile",
            hitstop_scale: 1.08,
            shake_scale: 1.05,
            feedback_priority_bonus: 6,
        },
        AttackPayloadId::ChickFreshEggDrop => AttackPayloadDef {
            id,
            kind: AttackKind::Special,
            shape_id: AttackShapeId::ProjectileBolt,
            reaction_family: ReactionFamilyId::LightAirPop,
            damage_profile: DamageProfileId::BasicStrike,
            element: DamageElement::Strike,
            power: 7.0,
            str_scale: 0.42,
            time_ms: 105,
            damage: 4.2,
            knockback: 3.1,
            vertical_knockback: 1.2,
            guardable: true,
            impact_cue: "impact_chick_fresh_egg_drop",
            hitstop_scale: 0.78,
            shake_scale: 0.7,
            feedback_priority_bonus: 2,
        },
        AttackPayloadId::ChickEggplantRoll => AttackPayloadDef {
            id,
            kind: AttackKind::Special,
            shape_id: AttackShapeId::ProjectileBolt,
            reaction_family: ReactionFamilyId::SlidingKnockdown,
            damage_profile: DamageProfileId::DashBody,
            element: DamageElement::Earth,
            power: 10.0,
            str_scale: 0.58,
            time_ms: 120,
            damage: 7.2,
            knockback: 5.9,
            vertical_knockback: 0.2,
            guardable: true,
            impact_cue: "impact_chick_eggplant_roll",
            hitstop_scale: 0.98,
            shake_scale: 0.92,
            feedback_priority_bonus: 5,
        },
        AttackPayloadId::ChickSunnySplash => AttackPayloadDef {
            id,
            kind: AttackKind::Special,
            shape_id: AttackShapeId::HazardField,
            reaction_family: ReactionFamilyId::ShortStandingStagger,
            damage_profile: DamageProfileId::Direct,
            element: DamageElement::Hazard,
            power: 4.0,
            str_scale: 0.28,
            time_ms: 105,
            damage: 2.4,
            knockback: 1.2,
            vertical_knockback: 0.5,
            guardable: true,
            impact_cue: "impact_chick_sunny_splash",
            hitstop_scale: 0.45,
            shake_scale: 0.34,
            feedback_priority_bonus: 1,
        },
        AttackPayloadId::ChickOmeletField => AttackPayloadDef {
            id,
            kind: AttackKind::Ultimate,
            shape_id: AttackShapeId::HazardField,
            reaction_family: ReactionFamilyId::ShortStandingStagger,
            damage_profile: DamageProfileId::Direct,
            element: DamageElement::Hazard,
            power: 6.0,
            str_scale: 0.38,
            time_ms: 110,
            damage: 4.5,
            knockback: 1.6,
            vertical_knockback: 0.8,
            guardable: true,
            impact_cue: "impact_chick_omelet_field",
            hitstop_scale: 0.52,
            shake_scale: 0.44,
            feedback_priority_bonus: 3,
        },
        AttackPayloadId::ChickShellScoot => AttackPayloadDef {
            id,
            kind: AttackKind::Dash,
            shape_id: AttackShapeId::GroundSkid,
            reaction_family: ReactionFamilyId::SlidingKnockdown,
            damage_profile: DamageProfileId::DashBody,
            element: DamageElement::Wind,
            power: 10.0,
            str_scale: 0.62,
            time_ms: 150,
            damage: DASH_ATTACK_DAMAGE * 0.72,
            knockback: DASH_ATTACK_KNOCKBACK * 0.74,
            vertical_knockback: 0.3,
            guardable: true,
            impact_cue: "impact_chick_shell_scoot",
            hitstop_scale: 1.02,
            shake_scale: 0.96,
            feedback_priority_bonus: 5,
        },
        AttackPayloadId::ChickShellScramble => AttackPayloadDef {
            id,
            kind: AttackKind::ComboFinisher,
            shape_id: AttackShapeId::CatBodySkid,
            reaction_family: ReactionFamilyId::GroundBounceDown,
            damage_profile: DamageProfileId::GroundBounce,
            element: DamageElement::Earth,
            power: 11.0,
            str_scale: 0.62,
            time_ms: 125,
            damage: COMBO_FINISHER_DAMAGE * 0.74,
            knockback: COMBO_FINISHER_KNOCKBACK * 0.78,
            vertical_knockback: 3.2,
            guardable: true,
            impact_cue: "impact_chick_shell_scramble",
            hitstop_scale: 1.08,
            shake_scale: 1.0,
            feedback_priority_bonus: 6,
        },
        AttackPayloadId::PenguinFishSlap1 => AttackPayloadDef {
            id,
            kind: AttackKind::Light1,
            shape_id: AttackShapeId::CompactSlashLead,
            reaction_family: ReactionFamilyId::ShortStandingStagger,
            damage_profile: DamageProfileId::BasicStrike,
            element: DamageElement::Wind,
            power: 9.0,
            str_scale: 0.75,
            time_ms: 95,
            damage: LIGHT_DAMAGE * 0.96,
            knockback: LIGHT_KNOCKBACK * 0.22,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_penguin_fish_slap_1",
            hitstop_scale: 1.0,
            shake_scale: 0.94,
            feedback_priority_bonus: 2,
        },
        AttackPayloadId::PenguinFishSlap2 => AttackPayloadDef {
            id,
            kind: AttackKind::Light2,
            shape_id: AttackShapeId::CompactSlashFollow,
            reaction_family: ReactionFamilyId::ShortStandingStagger,
            damage_profile: DamageProfileId::FollowupStrike,
            element: DamageElement::Wind,
            power: 9.0,
            str_scale: 0.75,
            time_ms: 100,
            damage: LIGHT2_DAMAGE * 0.98,
            knockback: LIGHT2_KNOCKBACK * 0.26,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_penguin_fish_slap_2",
            hitstop_scale: 1.05,
            shake_scale: 1.0,
            feedback_priority_bonus: 3,
        },
        AttackPayloadId::PenguinBellySlide => AttackPayloadDef {
            id,
            kind: AttackKind::ComboFinisher,
            shape_id: AttackShapeId::GroundSkid,
            reaction_family: ReactionFamilyId::SlidingKnockdown,
            damage_profile: DamageProfileId::DashBody,
            element: DamageElement::Wind,
            power: 12.0,
            str_scale: 0.8,
            time_ms: 130,
            damage: COMBO_FINISHER_DAMAGE * 0.86,
            knockback: COMBO_FINISHER_KNOCKBACK * 0.92,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_penguin_belly_slide",
            hitstop_scale: 1.16,
            shake_scale: 1.12,
            feedback_priority_bonus: 6,
        },
        AttackPayloadId::PenguinPanBonk => AttackPayloadDef {
            id,
            kind: AttackKind::Heavy,
            shape_id: AttackShapeId::ShoulderLine,
            reaction_family: ReactionFamilyId::HeavyStandingStagger,
            damage_profile: DamageProfileId::BasicStrike,
            element: DamageElement::Strike,
            power: 13.0,
            str_scale: 0.8,
            time_ms: 130,
            damage: HEAVY_DAMAGE * 0.72,
            knockback: HEAVY_KNOCKBACK * 0.4,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_penguin_pan_bonk",
            hitstop_scale: 1.24,
            shake_scale: 1.2,
            feedback_priority_bonus: 5,
        },
        AttackPayloadId::PenguinSledScoop => AttackPayloadDef {
            id,
            kind: AttackKind::Heavy,
            shape_id: AttackShapeId::RisingColumn,
            reaction_family: ReactionFamilyId::LauncherDown,
            damage_profile: DamageProfileId::LauncherCommit,
            element: DamageElement::Launch,
            power: 14.0,
            str_scale: 0.8,
            time_ms: 120,
            damage: HEAVY_DAMAGE * 0.96,
            knockback: HEAVY_KNOCKBACK * 0.68,
            vertical_knockback: 6.2,
            guardable: true,
            impact_cue: "impact_penguin_sled_scoop",
            hitstop_scale: 1.28,
            shake_scale: 1.24,
            feedback_priority_bonus: 8,
        },
        AttackPayloadId::PenguinSlopeCrash => AttackPayloadDef {
            id,
            kind: AttackKind::Dash,
            shape_id: AttackShapeId::PenguinSlopeBody,
            reaction_family: ReactionFamilyId::AirFishKnockdown,
            damage_profile: DamageProfileId::Direct,
            element: DamageElement::Launch,
            power: 5.0,
            str_scale: 0.22,
            time_ms: 540,
            damage: 1.2,
            knockback: 10.2,
            vertical_knockback: 4.1,
            guardable: true,
            impact_cue: "impact_penguin_slope_crash",
            hitstop_scale: 0.86,
            shake_scale: 1.04,
            feedback_priority_bonus: 6,
        },
        AttackPayloadId::PenguinIceSlide => AttackPayloadDef {
            id,
            kind: AttackKind::Dash,
            shape_id: AttackShapeId::GroundSkid,
            reaction_family: ReactionFamilyId::SlidingKnockdown,
            damage_profile: DamageProfileId::DashBody,
            element: DamageElement::Wind,
            power: 11.0,
            str_scale: 0.75,
            time_ms: 120,
            damage: DASH_ATTACK_DAMAGE * 0.92,
            knockback: DASH_ATTACK_KNOCKBACK * 0.9,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_penguin_ice_slide",
            hitstop_scale: 1.1,
            shake_scale: 1.06,
            feedback_priority_bonus: 5,
        },
        AttackPayloadId::PenguinPopsiclePeck => AttackPayloadDef {
            id,
            kind: AttackKind::Jump,
            shape_id: AttackShapeId::CompactThrust,
            reaction_family: ReactionFamilyId::ShortStandingStagger,
            damage_profile: DamageProfileId::BasicStrike,
            element: DamageElement::Strike,
            power: 11.0,
            str_scale: 0.75,
            time_ms: 260,
            damage: JUMP_ATTACK_DAMAGE * 0.86,
            knockback: JUMP_ATTACK_KNOCKBACK * 0.62,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_penguin_popsicle_peck",
            hitstop_scale: 1.02,
            shake_scale: 0.96,
            feedback_priority_bonus: 5,
        },
        AttackPayloadId::PenguinFrozenFishDive => AttackPayloadDef {
            id,
            kind: AttackKind::Heavy,
            shape_id: AttackShapeId::FallingSpikeArc,
            reaction_family: ReactionFamilyId::GroundBounceDown,
            damage_profile: DamageProfileId::AerialSpike,
            element: DamageElement::Wind,
            power: 14.0,
            str_scale: 0.8,
            time_ms: (JUMP_ATTACK_MAX_ACTIVE * 1000.0) as u32,
            damage: JUMP_HEAVY_DAMAGE * 1.08,
            knockback: JUMP_HEAVY_KNOCKBACK * 0.92,
            vertical_knockback: 3.0,
            guardable: true,
            impact_cue: "impact_penguin_frozen_fish_dive",
            hitstop_scale: 1.28,
            shake_scale: 1.24,
            feedback_priority_bonus: 8,
        },
        AttackPayloadId::GrabCatch => AttackPayloadDef {
            id,
            kind: AttackKind::Grab,
            shape_id: AttackShapeId::GrabCatch,
            reaction_family: ReactionFamilyId::GroundedDownGetup,
            damage_profile: DamageProfileId::GrabControl,
            element: DamageElement::Neutral,
            power: 5.0,
            str_scale: 0.8,
            time_ms: 100,
            damage: GRAB_DAMAGE,
            knockback: GRAB_KNOCKBACK,
            vertical_knockback: 0.0,
            guardable: false,
            impact_cue: "impact_grab_catch",
            hitstop_scale: 1.0,
            shake_scale: 0.9,
            feedback_priority_bonus: 4,
        },
        AttackPayloadId::DashStrike => AttackPayloadDef {
            id,
            kind: AttackKind::Dash,
            shape_id: AttackShapeId::ShoulderLine,
            reaction_family: ReactionFamilyId::MediumStandingStagger,
            damage_profile: DamageProfileId::DashBody,
            element: DamageElement::Wind,
            power: 11.0,
            str_scale: 0.7,
            time_ms: 100,
            damage: DASH_ATTACK_DAMAGE * 0.65,
            knockback: DASH_ATTACK_KNOCKBACK * 0.72,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_dash_shoulder_1",
            hitstop_scale: 1.08,
            shake_scale: 1.05,
            feedback_priority_bonus: 3,
        },
        AttackPayloadId::DashShoulderBeat => AttackPayloadDef {
            id,
            kind: AttackKind::Dash,
            shape_id: AttackShapeId::ShoulderLine,
            reaction_family: ReactionFamilyId::LightAirPop,
            damage_profile: DamageProfileId::DashBody,
            element: DamageElement::Wind,
            power: 11.0,
            str_scale: 0.7,
            time_ms: 120,
            damage: DASH_ATTACK_DAMAGE,
            knockback: DASH_ATTACK_KNOCKBACK,
            vertical_knockback: 3.0,
            guardable: true,
            impact_cue: "impact_dash_shoulder_2",
            hitstop_scale: 1.14,
            shake_scale: 1.16,
            feedback_priority_bonus: 5,
        },
        AttackPayloadId::JumpStrike => AttackPayloadDef {
            id,
            kind: AttackKind::Jump,
            shape_id: AttackShapeId::JumpKick,
            reaction_family: ReactionFamilyId::HeavyStandingStagger,
            damage_profile: DamageProfileId::BasicStrike,
            element: DamageElement::Strike,
            power: 11.0,
            str_scale: 0.8,
            time_ms: 100,
            damage: JUMP_ATTACK_DAMAGE * 0.72,
            knockback: JUMP_ATTACK_KNOCKBACK * 0.8,
            vertical_knockback: 0.0,
            guardable: true,
            impact_cue: "impact_jump_kick",
            hitstop_scale: 1.08,
            shake_scale: 1.08,
            feedback_priority_bonus: 3,
        },
        AttackPayloadId::JumpSpike => AttackPayloadDef {
            id,
            kind: AttackKind::Jump,
            shape_id: AttackShapeId::FallingSpikeArc,
            reaction_family: ReactionFamilyId::AerialSpikeDown,
            damage_profile: DamageProfileId::AerialSpike,
            element: DamageElement::Wind,
            power: 13.0,
            str_scale: 0.8,
            time_ms: (JUMP_ATTACK_MAX_ACTIVE * 1000.0) as u32,
            damage: JUMP_ATTACK_DAMAGE,
            knockback: JUMP_ATTACK_KNOCKBACK * 0.72,
            vertical_knockback: 2.4,
            guardable: true,
            impact_cue: "impact_jump_spike",
            hitstop_scale: 1.18,
            shake_scale: 1.18,
            feedback_priority_bonus: 6,
        },
        AttackPayloadId::JumpFishShot => AttackPayloadDef {
            id,
            kind: AttackKind::Heavy,
            shape_id: AttackShapeId::AirFishShot,
            reaction_family: ReactionFamilyId::AirFishKnockdown,
            damage_profile: DamageProfileId::GroundBounce,
            element: DamageElement::Wind,
            power: 13.0,
            str_scale: 0.8,
            time_ms: 520,
            damage: JUMP_HEAVY_DAMAGE,
            knockback: JUMP_HEAVY_KNOCKBACK,
            vertical_knockback: 2.2,
            guardable: true,
            impact_cue: "impact_jump_x_fish",
            hitstop_scale: 1.2,
            shake_scale: 1.24,
            feedback_priority_bonus: 7,
        },
        AttackPayloadId::DogJumpPounce => AttackPayloadDef {
            id,
            kind: AttackKind::Jump,
            shape_id: AttackShapeId::FallingSpikeArc,
            reaction_family: ReactionFamilyId::AerialSpikeDown,
            damage_profile: DamageProfileId::AerialSpike,
            element: DamageElement::Strike,
            power: 13.0,
            str_scale: 0.8,
            time_ms: (JUMP_ATTACK_MAX_ACTIVE * 1000.0) as u32,
            damage: JUMP_ATTACK_DAMAGE * 1.08,
            knockback: JUMP_ATTACK_KNOCKBACK * 0.78,
            vertical_knockback: 2.5,
            guardable: true,
            impact_cue: "impact_dog_jump_pounce",
            hitstop_scale: 1.22,
            shake_scale: 1.18,
            feedback_priority_bonus: 7,
        },
        AttackPayloadId::DogJumpFishShot => AttackPayloadDef {
            id,
            kind: AttackKind::Heavy,
            shape_id: AttackShapeId::AirFishShot,
            reaction_family: ReactionFamilyId::AirFishKnockdown,
            damage_profile: DamageProfileId::GroundBounce,
            element: DamageElement::Wind,
            power: 13.0,
            str_scale: 0.8,
            time_ms: 560,
            damage: JUMP_HEAVY_DAMAGE * 1.08,
            knockback: JUMP_HEAVY_KNOCKBACK * 1.04,
            vertical_knockback: 2.3,
            guardable: true,
            impact_cue: "impact_dog_jump_fish",
            hitstop_scale: 1.24,
            shake_scale: 1.28,
            feedback_priority_bonus: 8,
        },
        AttackPayloadId::FoxJumpSwipe => AttackPayloadDef {
            id,
            kind: AttackKind::Jump,
            shape_id: AttackShapeId::HookSweep,
            reaction_family: ReactionFamilyId::SlidingKnockdown,
            damage_profile: DamageProfileId::AerialSpike,
            element: DamageElement::Wind,
            power: 11.0,
            str_scale: 0.7,
            time_ms: (JUMP_ATTACK_MAX_ACTIVE * 1000.0) as u32,
            damage: JUMP_ATTACK_DAMAGE * 0.86,
            knockback: JUMP_ATTACK_KNOCKBACK * 0.88,
            vertical_knockback: 1.8,
            guardable: true,
            impact_cue: "impact_fox_jump_swipe",
            hitstop_scale: 0.94,
            shake_scale: 0.88,
            feedback_priority_bonus: 5,
        },
        AttackPayloadId::FoxJumpFishShot => AttackPayloadDef {
            id,
            kind: AttackKind::Heavy,
            shape_id: AttackShapeId::AirFishShot,
            reaction_family: ReactionFamilyId::AirFishKnockdown,
            damage_profile: DamageProfileId::GroundBounce,
            element: DamageElement::Wind,
            power: 11.0,
            str_scale: 0.7,
            time_ms: 460,
            damage: JUMP_HEAVY_DAMAGE * 0.9,
            knockback: JUMP_HEAVY_KNOCKBACK * 1.12,
            vertical_knockback: 1.8,
            guardable: true,
            impact_cue: "impact_fox_jump_fish",
            hitstop_scale: 1.04,
            shake_scale: 1.0,
            feedback_priority_bonus: 6,
        },
        AttackPayloadId::PandaJumpDrop => AttackPayloadDef {
            id,
            kind: AttackKind::Jump,
            shape_id: AttackShapeId::FallingSpikeArc,
            reaction_family: ReactionFamilyId::GroundBounceDown,
            damage_profile: DamageProfileId::AerialSpike,
            element: DamageElement::Earth,
            power: 15.0,
            str_scale: 0.8,
            time_ms: (JUMP_ATTACK_MAX_ACTIVE * 1000.0) as u32,
            damage: JUMP_ATTACK_DAMAGE * 1.25,
            knockback: JUMP_ATTACK_KNOCKBACK * 0.82,
            vertical_knockback: 3.0,
            guardable: true,
            impact_cue: "impact_panda_jump_drop",
            hitstop_scale: 1.4,
            shake_scale: 1.42,
            feedback_priority_bonus: 9,
        },
        AttackPayloadId::PandaJumpFishShot => AttackPayloadDef {
            id,
            kind: AttackKind::Heavy,
            shape_id: AttackShapeId::AirFishShot,
            reaction_family: ReactionFamilyId::AirFishKnockdown,
            damage_profile: DamageProfileId::GroundBounce,
            element: DamageElement::Earth,
            power: 15.0,
            str_scale: 0.8,
            time_ms: 640,
            damage: JUMP_HEAVY_DAMAGE * 1.18,
            knockback: JUMP_HEAVY_KNOCKBACK * 0.96,
            vertical_knockback: 2.8,
            guardable: true,
            impact_cue: "impact_panda_jump_fish",
            hitstop_scale: 1.38,
            shake_scale: 1.42,
            feedback_priority_bonus: 9,
        },
        AttackPayloadId::PigJumpBellyDrop => AttackPayloadDef {
            id,
            kind: AttackKind::Jump,
            shape_id: AttackShapeId::FallingSpikeArc,
            reaction_family: ReactionFamilyId::GroundBounceDown,
            damage_profile: DamageProfileId::AerialSpike,
            element: DamageElement::Earth,
            power: 15.0,
            str_scale: 0.8,
            time_ms: (JUMP_ATTACK_MAX_ACTIVE * 1000.0) as u32,
            damage: JUMP_ATTACK_DAMAGE * 1.36,
            knockback: JUMP_ATTACK_KNOCKBACK * 0.86,
            vertical_knockback: 3.2,
            guardable: true,
            impact_cue: "impact_pig_jump_belly",
            hitstop_scale: 1.48,
            shake_scale: 1.55,
            feedback_priority_bonus: 10,
        },
        AttackPayloadId::PigHamLob => AttackPayloadDef {
            id,
            kind: AttackKind::Heavy,
            shape_id: AttackShapeId::PigHamLob,
            reaction_family: ReactionFamilyId::AirFishKnockdown,
            damage_profile: DamageProfileId::GroundBounce,
            element: DamageElement::Earth,
            power: 15.0,
            str_scale: 0.8,
            time_ms: 700,
            damage: JUMP_HEAVY_DAMAGE * 1.26,
            knockback: JUMP_HEAVY_KNOCKBACK * 1.08,
            vertical_knockback: 3.0,
            guardable: true,
            impact_cue: "impact_pig_jump_ham",
            hitstop_scale: 1.5,
            shake_scale: 1.58,
            feedback_priority_bonus: 11,
        },
        AttackPayloadId::GuardCounter => AttackPayloadDef {
            id,
            kind: AttackKind::GuardCounter,
            shape_id: AttackShapeId::CounterArc,
            reaction_family: ReactionFamilyId::CounterPop,
            damage_profile: DamageProfileId::CounterBlow,
            element: DamageElement::Shock,
            power: 10.0,
            str_scale: 0.8,
            time_ms: 100,
            damage: GUARD_COUNTER_DAMAGE,
            knockback: GUARD_COUNTER_KNOCKBACK,
            vertical_knockback: 2.6,
            guardable: true,
            impact_cue: "impact_guard_counter",
            hitstop_scale: 1.16,
            shake_scale: 1.12,
            feedback_priority_bonus: 5,
        },
        AttackPayloadId::SpecialProjectile => AttackPayloadDef {
            id,
            kind: AttackKind::Special,
            shape_id: AttackShapeId::ProjectileBolt,
            reaction_family: ReactionFamilyId::ShortStandingStagger,
            damage_profile: DamageProfileId::BasicStrike,
            element: DamageElement::Shock,
            power: 9.0,
            str_scale: 0.8,
            time_ms: 100,
            damage: SPECIAL_PROJECTILE_DAMAGE,
            knockback: SPECIAL_PROJECTILE_KNOCKBACK,
            vertical_knockback: 2.2,
            guardable: true,
            impact_cue: "impact_special_projectile",
            hitstop_scale: 0.92,
            shake_scale: 0.85,
            feedback_priority_bonus: 2,
        },
        AttackPayloadId::SpecialTrap => AttackPayloadDef {
            id,
            kind: AttackKind::Special,
            shape_id: AttackShapeId::TrapPlate,
            reaction_family: ReactionFamilyId::GroundedDownGetup,
            damage_profile: DamageProfileId::GroundBounce,
            element: DamageElement::Earth,
            power: 10.0,
            str_scale: 0.7,
            time_ms: 160,
            damage: SPECIAL_TRAP_DAMAGE,
            knockback: SPECIAL_TRAP_KNOCKBACK,
            vertical_knockback: 2.6,
            guardable: false,
            impact_cue: "impact_special_trap",
            hitstop_scale: 1.1,
            shake_scale: 1.05,
            feedback_priority_bonus: 5,
        },
        AttackPayloadId::SpecialShockwave => AttackPayloadDef {
            id,
            kind: AttackKind::Special,
            shape_id: AttackShapeId::ShockwaveRing,
            reaction_family: ReactionFamilyId::GroundedDownGetup,
            damage_profile: DamageProfileId::DashBody,
            element: DamageElement::Wind,
            power: 11.0,
            str_scale: 0.8,
            time_ms: 200,
            damage: SPECIAL_SHOCKWAVE_DAMAGE,
            knockback: SPECIAL_SHOCKWAVE_KNOCKBACK,
            vertical_knockback: 2.8,
            guardable: true,
            impact_cue: "impact_special_shockwave",
            hitstop_scale: 1.12,
            shake_scale: 1.16,
            feedback_priority_bonus: 6,
        },
        AttackPayloadId::SpecialHazard => AttackPayloadDef {
            id,
            kind: AttackKind::Special,
            shape_id: AttackShapeId::HazardField,
            reaction_family: ReactionFamilyId::ShortStandingStagger,
            damage_profile: DamageProfileId::Direct,
            element: DamageElement::Hazard,
            power: 7.0,
            str_scale: 0.7,
            time_ms: 120,
            damage: SPECIAL_HAZARD_DAMAGE,
            knockback: SPECIAL_HAZARD_KNOCKBACK,
            vertical_knockback: 1.8,
            guardable: true,
            impact_cue: "impact_special_hazard",
            hitstop_scale: 0.82,
            shake_scale: 0.78,
            feedback_priority_bonus: 1,
        },
        AttackPayloadId::ItemThrowLight => AttackPayloadDef {
            id,
            kind: AttackKind::ItemThrow,
            shape_id: AttackShapeId::ItemLob,
            reaction_family: ReactionFamilyId::LightAirPop,
            damage_profile: DamageProfileId::ItemHeavy,
            element: DamageElement::Strike,
            power: 9.0,
            str_scale: 0.8,
            time_ms: 120,
            damage: ITEM_THROW_DAMAGE * 0.8,
            knockback: ITEM_THROW_KNOCKBACK * 0.78,
            vertical_knockback: 2.6,
            guardable: true,
            impact_cue: "impact_item_throw_light",
            hitstop_scale: 1.0,
            shake_scale: 0.95,
            feedback_priority_bonus: 3,
        },
        AttackPayloadId::ItemThrowHeavy => AttackPayloadDef {
            id,
            kind: AttackKind::ItemThrow,
            shape_id: AttackShapeId::ItemLob,
            reaction_family: ReactionFamilyId::GroundedDownGetup,
            damage_profile: DamageProfileId::ItemHeavy,
            element: DamageElement::Earth,
            power: 13.0,
            str_scale: 0.8,
            time_ms: 140,
            damage: ITEM_THROW_DAMAGE,
            knockback: ITEM_THROW_KNOCKBACK,
            vertical_knockback: 3.4,
            guardable: true,
            impact_cue: "impact_item_throw_heavy",
            hitstop_scale: 1.16,
            shake_scale: 1.18,
            feedback_priority_bonus: 6,
        },
        AttackPayloadId::BombBlast => AttackPayloadDef {
            id,
            kind: AttackKind::ItemBlast,
            shape_id: AttackShapeId::BombBurst,
            reaction_family: ReactionFamilyId::LauncherDown,
            damage_profile: DamageProfileId::LauncherCommit,
            element: DamageElement::Blast,
            power: 15.0,
            str_scale: 0.8,
            time_ms: 200,
            damage: POP_BOMB_DAMAGE,
            knockback: POP_BOMB_KNOCKBACK,
            vertical_knockback: 3.3,
            guardable: true,
            impact_cue: "impact_pop_bomb",
            hitstop_scale: 1.22,
            shake_scale: 1.28,
            feedback_priority_bonus: 7,
        },
    }
}

pub fn technique_id_for_action(action: FighterAction) -> Option<TechniqueId> {
    Some(match action {
        FighterAction::GrabStartup => TechniqueId::Grab,
        FighterAction::Throwing => TechniqueId::Throw,
        FighterAction::GuardCounter => TechniqueId::GuardCounter,
        FighterAction::SpecialCast => TechniqueId::SpecialCast,
        FighterAction::ItemPickup => TechniqueId::ItemPickup,
        FighterAction::ItemSwing => TechniqueId::ItemSwing,
        FighterAction::ItemThrow => TechniqueId::ItemThrow,
        FighterAction::ItemDrop => TechniqueId::ItemDrop,
        FighterAction::GuardStep => TechniqueId::GuardStep,
        FighterAction::QuickStand => TechniqueId::QuickStand,
        FighterAction::RecoveryRoll => TechniqueId::RecoveryRoll,
        FighterAction::LandingRecovery => TechniqueId::LandingRecovery,
        _ => return None,
    })
}

pub fn technique_definition(action: FighterAction) -> Option<TechniqueDefinition> {
    technique_definition_by_id(technique_id_for_action(action)?)
}

pub fn technique_definition_by_id(id: TechniqueId) -> Option<TechniqueDefinition> {
    Some(match id {
        TechniqueId::CatLight1 => TechniqueDefinition {
            id,
            action: FighterAction::LightAttack1,
            button: TechniqueButton::A,
            status: TechniqueStatus::Grounded,
            script: script("tataki1.sc", Some(686), Some(720), 800, &LIGHT1_EVENTS),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: Some(MsTimingWindow::closed(200, 600)),
            branch_window: Some(MsTimingWindow::closed(200, 600)),
            chain_rule: None,
        },
        TechniqueId::CatLight2 => TechniqueDefinition {
            id,
            action: FighterAction::LightAttack2,
            button: TechniqueButton::A,
            status: TechniqueStatus::Grounded,
            script: script("tataki2.sc", Some(650), Some(690), 850, &LIGHT2_EVENTS),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: Some(MsTimingWindow::closed(240, 620)),
            branch_window: Some(MsTimingWindow::closed(240, 620)),
            chain_rule: Some(TechniqueChainRule {
                previous: PrevTechExpr {
                    any_of: A_S_PREV,
                    command: Some(TechniqueButton::A),
                    conditions: A_CHAIN_CONDITIONS,
                },
                window: MsTimingWindow::closed(200, 600),
                same_button_required: true,
            }),
        },
        TechniqueId::CatComboFinisher => TechniqueDefinition {
            id,
            action: FighterAction::ComboFinisher,
            button: TechniqueButton::A,
            status: TechniqueStatus::Grounded,
            script: script(
                "combo_finisher.sc",
                Some(450),
                None,
                900,
                &COMBO_FINISHER_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: Some(TechniqueChainRule {
                previous: PrevTechExpr {
                    any_of: A_SS_PREV,
                    command: Some(TechniqueButton::A),
                    conditions: A_FINISHER_CONDITIONS,
                },
                window: MsTimingWindow::closed(240, 620),
                same_button_required: true,
            }),
        },
        TechniqueId::CatDashComboFinisher => TechniqueDefinition {
            id,
            action: FighterAction::ComboFinisher,
            button: TechniqueButton::A,
            status: TechniqueStatus::Grounded,
            script: script(
                "dash_combo_finisher.sc",
                Some(450),
                None,
                900,
                &DASH_COMBO_FINISHER_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::CatHeavy => TechniqueDefinition {
            id,
            action: FighterAction::HeavyAttack,
            button: TechniqueButton::B,
            status: TechniqueStatus::Grounded,
            script: script(
                "heavy_step.sc",
                Some(300),
                Some(260),
                430,
                &HEAVY_STEP_EVENTS,
            ),
            input_buffer_ms: 320,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: Some(MsTimingWindow::closed(0, 380)),
            branch_window: Some(MsTimingWindow::closed(0, 380)),
            chain_rule: None,
        },
        TechniqueId::CatHeavy2 => TechniqueDefinition {
            id,
            action: FighterAction::HeavyAttack2,
            button: TechniqueButton::B,
            status: TechniqueStatus::Grounded,
            script: script("kiriage.sc", Some(500), Some(650), 960, &KIRIAGE_EVENTS),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: Some(TechniqueChainRule {
                previous: PrevTechExpr {
                    any_of: X_STEP_PREV,
                    command: Some(TechniqueButton::B),
                    conditions: X_CHAIN_CONDITIONS,
                },
                window: MsTimingWindow::closed(0, 380),
                same_button_required: true,
            }),
        },
        TechniqueId::PigLight1 => TechniqueDefinition {
            id,
            action: FighterAction::LightAttack1,
            button: TechniqueButton::A,
            status: TechniqueStatus::Grounded,
            script: script(
                "pig_cat_light_1.sc",
                Some(686),
                Some(720),
                800,
                &PIG_LIGHT1_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: Some(MsTimingWindow::closed(200, 600)),
            branch_window: Some(MsTimingWindow::closed(200, 600)),
            chain_rule: None,
        },
        TechniqueId::PigLight2 => TechniqueDefinition {
            id,
            action: FighterAction::LightAttack2,
            button: TechniqueButton::A,
            status: TechniqueStatus::Grounded,
            script: script(
                "pig_cat_light_2.sc",
                Some(650),
                Some(690),
                850,
                &PIG_LIGHT2_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: Some(MsTimingWindow::closed(240, 620)),
            branch_window: Some(MsTimingWindow::closed(240, 620)),
            chain_rule: Some(TechniqueChainRule {
                previous: PrevTechExpr {
                    any_of: PIG_A_S_PREV,
                    command: Some(TechniqueButton::A),
                    conditions: A_CHAIN_CONDITIONS,
                },
                window: MsTimingWindow::closed(200, 600),
                same_button_required: true,
            }),
        },
        TechniqueId::PigComboFinisher => TechniqueDefinition {
            id,
            action: FighterAction::ComboFinisher,
            button: TechniqueButton::A,
            status: TechniqueStatus::Grounded,
            script: script(
                "pig_ham_slam.sc",
                Some(900),
                None,
                1280,
                &PIG_COMBO_FINISHER_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: Some(TechniqueChainRule {
                previous: PrevTechExpr {
                    any_of: PIG_A_SS_PREV,
                    command: Some(TechniqueButton::A),
                    conditions: A_FINISHER_CONDITIONS,
                },
                window: MsTimingWindow::closed(150, 320),
                same_button_required: true,
            }),
        },
        TechniqueId::PigHeavy => TechniqueDefinition {
            id,
            action: FighterAction::HeavyAttack,
            button: TechniqueButton::B,
            status: TechniqueStatus::Grounded,
            script: script(
                "pig_half_circle_swing.sc",
                Some(1040),
                None,
                1280,
                &PIG_HEAVY_EVENTS,
            ),
            input_buffer_ms: 260,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: Some(MsTimingWindow::closed(160, 680)),
            branch_window: Some(MsTimingWindow::closed(160, 680)),
            chain_rule: None,
        },
        TechniqueId::PigHeavy2 => TechniqueDefinition {
            id,
            action: FighterAction::HeavyAttack2,
            button: TechniqueButton::B,
            status: TechniqueStatus::Grounded,
            script: script(
                "pig_ham_launcher.sc",
                Some(900),
                Some(1060),
                1380,
                &PIG_HEAVY2_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: Some(TechniqueChainRule {
                previous: PrevTechExpr {
                    any_of: PIG_X_STEP_PREV,
                    command: Some(TechniqueButton::B),
                    conditions: X_CHAIN_CONDITIONS,
                },
                window: MsTimingWindow::closed(160, 680),
                same_button_required: true,
            }),
        },
        TechniqueId::PigJumpAttack => TechniqueDefinition {
            id,
            action: FighterAction::JumpAttack,
            button: TechniqueButton::A,
            status: TechniqueStatus::Airborne,
            script: script(
                "pig_air_belly_drop.sc",
                None,
                None,
                760,
                &PIG_JUMP_ATTACK_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::PigJumpHeavy => TechniqueDefinition {
            id,
            action: FighterAction::JumpHeavyAttack,
            button: TechniqueButton::B,
            status: TechniqueStatus::Airborne,
            script: script("pig_air_ham.sc", None, None, 820, &PIG_JUMP_HEAVY_EVENTS),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::PigUltimateStartup => TechniqueDefinition {
            id,
            action: FighterAction::UltimateStartup,
            button: TechniqueButton::Ultimate,
            status: TechniqueStatus::Grounded,
            script: script(
                "pig_unblockable_grab_startup.sc",
                Some(1600),
                None,
                2200,
                &PIG_ULTIMATE_STARTUP_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: ULTIMATE_STAMINA_COST,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::PigUltimateRush => TechniqueDefinition {
            id,
            action: FighterAction::UltimateRush,
            button: TechniqueButton::Ultimate,
            status: TechniqueStatus::Grounded,
            script: script(
                "pig_unblockable_grab_rush.sc",
                Some(1440),
                None,
                1860,
                &PIG_ULTIMATE_RUSH_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::DogLight1 => TechniqueDefinition {
            id,
            action: FighterAction::LightAttack1,
            button: TechniqueButton::A,
            status: TechniqueStatus::Grounded,
            script: script(
                "dog_bite_1.sc",
                Some(500),
                Some(530),
                620,
                &DOG_LIGHT1_EVENTS,
            ),
            input_buffer_ms: 180,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: Some(MsTimingWindow::closed(180, 520)),
            branch_window: Some(MsTimingWindow::closed(180, 520)),
            chain_rule: None,
        },
        TechniqueId::DogLight2 => TechniqueDefinition {
            id,
            action: FighterAction::LightAttack2,
            button: TechniqueButton::A,
            status: TechniqueStatus::Grounded,
            script: script(
                "dog_bite_2.sc",
                Some(530),
                Some(560),
                680,
                &DOG_LIGHT2_EVENTS,
            ),
            input_buffer_ms: 180,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: Some(MsTimingWindow::closed(190, 560)),
            branch_window: Some(MsTimingWindow::closed(190, 560)),
            chain_rule: Some(TechniqueChainRule {
                previous: PrevTechExpr {
                    any_of: DOG_A_S_PREV,
                    command: Some(TechniqueButton::A),
                    conditions: A_CHAIN_CONDITIONS,
                },
                window: MsTimingWindow::closed(180, 520),
                same_button_required: true,
            }),
        },
        TechniqueId::DogComboFinisher => TechniqueDefinition {
            id,
            action: FighterAction::ComboFinisher,
            button: TechniqueButton::A,
            status: TechniqueStatus::Grounded,
            script: script(
                "dog_body_pounce.sc",
                Some(610),
                None,
                980,
                &DOG_COMBO_FINISHER_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: Some(TechniqueChainRule {
                previous: PrevTechExpr {
                    any_of: DOG_A_SS_PREV,
                    command: Some(TechniqueButton::A),
                    conditions: A_FINISHER_CONDITIONS,
                },
                window: MsTimingWindow::closed(190, 560),
                same_button_required: true,
            }),
        },
        TechniqueId::DogHeavy => TechniqueDefinition {
            id,
            action: FighterAction::HeavyAttack,
            button: TechniqueButton::B,
            status: TechniqueStatus::Grounded,
            script: script(
                "dog_shoulder_step.sc",
                Some(420),
                Some(455),
                620,
                &DOG_HEAVY_EVENTS,
            ),
            input_buffer_ms: 240,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: Some(MsTimingWindow::closed(80, 455)),
            branch_window: Some(MsTimingWindow::closed(80, 455)),
            chain_rule: None,
        },
        TechniqueId::DogHeavy2 => TechniqueDefinition {
            id,
            action: FighterAction::HeavyAttack2,
            button: TechniqueButton::B,
            status: TechniqueStatus::Grounded,
            script: script(
                "dog_launch_bite.sc",
                Some(590),
                Some(720),
                1050,
                &DOG_HEAVY2_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: Some(TechniqueChainRule {
                previous: PrevTechExpr {
                    any_of: DOG_X_STEP_PREV,
                    command: Some(TechniqueButton::B),
                    conditions: X_CHAIN_CONDITIONS,
                },
                window: MsTimingWindow::closed(80, 455),
                same_button_required: true,
            }),
        },
        TechniqueId::DogJumpAttack => TechniqueDefinition {
            id,
            action: FighterAction::JumpAttack,
            button: TechniqueButton::A,
            status: TechniqueStatus::Airborne,
            script: script(
                "dog_air_pounce.sc",
                None,
                None,
                560,
                &DOG_JUMP_ATTACK_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::DogJumpHeavy => TechniqueDefinition {
            id,
            action: FighterAction::JumpHeavyAttack,
            button: TechniqueButton::B,
            status: TechniqueStatus::Airborne,
            script: script("dog_air_fish.sc", None, None, 620, &DOG_JUMP_HEAVY_EVENTS),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::DogUltimateStartup => TechniqueDefinition {
            id,
            action: FighterAction::UltimateStartup,
            button: TechniqueButton::Ultimate,
            status: TechniqueStatus::Grounded,
            script: script(
                "dog_ultimate_startup.sc",
                Some(530),
                None,
                720,
                &DOG_ULTIMATE_STARTUP_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: ULTIMATE_STAMINA_COST,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::DogUltimateRush => TechniqueDefinition {
            id,
            action: FighterAction::UltimateRush,
            button: TechniqueButton::Ultimate,
            status: TechniqueStatus::Grounded,
            script: script(
                "dog_ultimate_rush.sc",
                Some(1100),
                None,
                1390,
                &DOG_ULTIMATE_RUSH_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::FoxLight1 => TechniqueDefinition {
            id,
            action: FighterAction::LightAttack1,
            button: TechniqueButton::A,
            status: TechniqueStatus::Grounded,
            script: script(
                "fox_swipe_1.sc",
                Some(360),
                Some(390),
                470,
                &FOX_LIGHT1_EVENTS,
            ),
            input_buffer_ms: 220,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: Some(MsTimingWindow::closed(90, 360)),
            branch_window: Some(MsTimingWindow::closed(90, 360)),
            chain_rule: None,
        },
        TechniqueId::FoxLight2 => TechniqueDefinition {
            id,
            action: FighterAction::LightAttack2,
            button: TechniqueButton::A,
            status: TechniqueStatus::Grounded,
            script: script(
                "fox_swipe_2.sc",
                Some(340),
                Some(370),
                460,
                &FOX_LIGHT2_EVENTS,
            ),
            input_buffer_ms: 220,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: Some(MsTimingWindow::closed(90, 370)),
            branch_window: Some(MsTimingWindow::closed(90, 370)),
            chain_rule: Some(TechniqueChainRule {
                previous: PrevTechExpr {
                    any_of: FOX_A_S_PREV,
                    command: Some(TechniqueButton::A),
                    conditions: A_CHAIN_CONDITIONS,
                },
                window: MsTimingWindow::closed(90, 360),
                same_button_required: true,
            }),
        },
        TechniqueId::FoxComboFinisher => TechniqueDefinition {
            id,
            action: FighterAction::ComboFinisher,
            button: TechniqueButton::A,
            status: TechniqueStatus::Grounded,
            script: script(
                "fox_tail_sweep.sc",
                Some(430),
                None,
                700,
                &FOX_COMBO_FINISHER_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: Some(TechniqueChainRule {
                previous: PrevTechExpr {
                    any_of: FOX_A_SS_PREV,
                    command: Some(TechniqueButton::A),
                    conditions: A_FINISHER_CONDITIONS,
                },
                window: MsTimingWindow::closed(90, 370),
                same_button_required: true,
            }),
        },
        TechniqueId::FoxHeavy => TechniqueDefinition {
            id,
            action: FighterAction::HeavyAttack,
            button: TechniqueButton::B,
            status: TechniqueStatus::Grounded,
            script: script(
                "fox_skitter_step.sc",
                Some(230),
                Some(250),
                360,
                &FOX_HEAVY_EVENTS,
            ),
            input_buffer_ms: 260,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: Some(MsTimingWindow::closed(0, 250)),
            branch_window: Some(MsTimingWindow::closed(0, 250)),
            chain_rule: None,
        },
        TechniqueId::FoxHeavy2 => TechniqueDefinition {
            id,
            action: FighterAction::HeavyAttack2,
            button: TechniqueButton::B,
            status: TechniqueStatus::Grounded,
            script: script(
                "fox_flip_launch.sc",
                Some(390),
                Some(500),
                760,
                &FOX_HEAVY2_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: Some(TechniqueChainRule {
                previous: PrevTechExpr {
                    any_of: FOX_X_STEP_PREV,
                    command: Some(TechniqueButton::B),
                    conditions: X_CHAIN_CONDITIONS,
                },
                window: MsTimingWindow::closed(0, 250),
                same_button_required: true,
            }),
        },
        TechniqueId::FoxJumpAttack => TechniqueDefinition {
            id,
            action: FighterAction::JumpAttack,
            button: TechniqueButton::A,
            status: TechniqueStatus::Airborne,
            script: script("fox_air_swipe.sc", None, None, 410, &FOX_JUMP_ATTACK_EVENTS),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::FoxJumpHeavy => TechniqueDefinition {
            id,
            action: FighterAction::JumpHeavyAttack,
            button: TechniqueButton::B,
            status: TechniqueStatus::Airborne,
            script: script("fox_air_fish.sc", None, None, 480, &FOX_JUMP_HEAVY_EVENTS),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::FoxUltimateStartup => TechniqueDefinition {
            id,
            action: FighterAction::UltimateStartup,
            button: TechniqueButton::Ultimate,
            status: TechniqueStatus::Grounded,
            script: script(
                "fox_ultimate_startup.sc",
                Some(390),
                None,
                560,
                &FOX_ULTIMATE_STARTUP_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: ULTIMATE_STAMINA_COST,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::FoxUltimateRush => TechniqueDefinition {
            id,
            action: FighterAction::UltimateRush,
            button: TechniqueButton::Ultimate,
            status: TechniqueStatus::Grounded,
            script: script(
                "fox_ultimate_rush.sc",
                Some(790),
                None,
                1040,
                &FOX_ULTIMATE_RUSH_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::BeeLight1 => TechniqueDefinition {
            id,
            action: FighterAction::LightAttack1,
            button: TechniqueButton::A,
            status: TechniqueStatus::Grounded,
            script: script(
                "bee_worker_swarm.sc",
                Some(220),
                None,
                360,
                &BEE_LIGHT1_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::BeeLight2 => TechniqueDefinition {
            id,
            action: FighterAction::LightAttack2,
            button: TechniqueButton::A,
            status: TechniqueStatus::Grounded,
            script: script(
                "bee_cross_sting.sc",
                Some(350),
                Some(380),
                470,
                &BEE_LIGHT2_EVENTS,
            ),
            input_buffer_ms: 220,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: Some(MsTimingWindow::closed(90, 380)),
            branch_window: Some(MsTimingWindow::closed(90, 380)),
            chain_rule: Some(TechniqueChainRule {
                previous: PrevTechExpr {
                    any_of: BEE_A_S_PREV,
                    command: Some(TechniqueButton::A),
                    conditions: A_CHAIN_CONDITIONS,
                },
                window: MsTimingWindow::closed(90, 370),
                same_button_required: true,
            }),
        },
        TechniqueId::BeeComboFinisher => TechniqueDefinition {
            id,
            action: FighterAction::ComboFinisher,
            button: TechniqueButton::A,
            status: TechniqueStatus::Grounded,
            script: script(
                "bee_spiral_sting.sc",
                Some(470),
                None,
                760,
                &BEE_COMBO_FINISHER_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: Some(TechniqueChainRule {
                previous: PrevTechExpr {
                    any_of: BEE_A_SS_PREV,
                    command: Some(TechniqueButton::A),
                    conditions: A_FINISHER_CONDITIONS,
                },
                window: MsTimingWindow::closed(90, 380),
                same_button_required: true,
            }),
        },
        TechniqueId::BeeHeavy => TechniqueDefinition {
            id,
            action: FighterAction::HeavyAttack,
            button: TechniqueButton::B,
            status: TechniqueStatus::Grounded,
            script: script(
                "bee_piercing_step.sc",
                Some(260),
                Some(290),
                410,
                &BEE_HEAVY_EVENTS,
            ),
            input_buffer_ms: 280,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: Some(MsTimingWindow::closed(0, 320)),
            branch_window: Some(MsTimingWindow::closed(0, 320)),
            chain_rule: None,
        },
        TechniqueId::BeeHeavy2 => TechniqueDefinition {
            id,
            action: FighterAction::HeavyAttack2,
            button: TechniqueButton::B,
            status: TechniqueStatus::Grounded,
            script: script(
                "bee_homing_sting.sc",
                Some(320),
                None,
                560,
                &BEE_HEAVY2_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::BeeJumpAttack => TechniqueDefinition {
            id,
            action: FighterAction::JumpAttack,
            button: TechniqueButton::A,
            status: TechniqueStatus::Airborne,
            script: script(
                "bee_air_dash.sc",
                Some(165),
                None,
                420,
                &BEE_JUMP_ATTACK_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::BeeJumpHeavy => TechniqueDefinition {
            id,
            action: FighterAction::JumpHeavyAttack,
            button: TechniqueButton::B,
            status: TechniqueStatus::Airborne,
            script: script("bee_hive_dive.sc", None, None, 620, &BEE_JUMP_HEAVY_EVENTS),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::BeeUltimateStartup => TechniqueDefinition {
            id,
            action: FighterAction::UltimateStartup,
            button: TechniqueButton::Ultimate,
            status: TechniqueStatus::Grounded,
            script: script(
                "bee_ultimate_swarm.sc",
                Some(360),
                None,
                620,
                &BEE_ULTIMATE_STARTUP_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: ULTIMATE_STAMINA_COST,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::BeeLegacyUltimateStartup => TechniqueDefinition {
            id,
            action: FighterAction::UltimateStartup,
            button: TechniqueButton::Ultimate,
            status: TechniqueStatus::Grounded,
            script: script(
                "bee_legacy_ultimate_startup.sc",
                Some(860),
                None,
                1040,
                &BEE_LEGACY_ULTIMATE_STARTUP_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: ULTIMATE_STAMINA_COST,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::BeeLegacyUltimateRush => TechniqueDefinition {
            id,
            action: FighterAction::UltimateRush,
            button: TechniqueButton::Ultimate,
            status: TechniqueStatus::Grounded,
            script: script(
                "bee_legacy_ultimate_rush.sc",
                Some(930),
                None,
                1180,
                &BEE_LEGACY_ULTIMATE_RUSH_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::PenguinLight1 => TechniqueDefinition {
            id,
            action: FighterAction::LightAttack1,
            button: TechniqueButton::A,
            status: TechniqueStatus::Grounded,
            script: script(
                "penguin_snowflake_shot.sc",
                Some(150),
                Some(150),
                160,
                &PENGUIN_LIGHT1_EVENTS,
            ),
            input_buffer_ms: 160,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: Some(MsTimingWindow::closed(140, 150)),
            branch_window: Some(MsTimingWindow::closed(140, 150)),
            chain_rule: None,
        },
        TechniqueId::PenguinLight2 => TechniqueDefinition {
            id,
            action: FighterAction::LightAttack2,
            button: TechniqueButton::A,
            status: TechniqueStatus::Grounded,
            script: script(
                "penguin_snowflake_followup.sc",
                Some(165),
                Some(165),
                175,
                &PENGUIN_LIGHT2_EVENTS,
            ),
            input_buffer_ms: 160,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: Some(MsTimingWindow::closed(155, 165)),
            branch_window: Some(MsTimingWindow::closed(155, 165)),
            chain_rule: Some(TechniqueChainRule {
                previous: PrevTechExpr {
                    any_of: PENGUIN_A_S_PREV,
                    command: Some(TechniqueButton::A),
                    conditions: A_CHAIN_CONDITIONS,
                },
                window: MsTimingWindow::closed(140, 150),
                same_button_required: true,
            }),
        },
        TechniqueId::PenguinComboFinisher => TechniqueDefinition {
            id,
            action: FighterAction::ComboFinisher,
            button: TechniqueButton::A,
            status: TechniqueStatus::Grounded,
            script: script(
                "penguin_belly_slide.sc",
                Some(620),
                None,
                980,
                &PENGUIN_COMBO_FINISHER_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::PenguinHeavy => TechniqueDefinition {
            id,
            action: FighterAction::HeavyAttack,
            button: TechniqueButton::B,
            status: TechniqueStatus::Grounded,
            script: script(
                "penguin_snowman_drop.sc",
                Some(500),
                Some(540),
                760,
                &PENGUIN_HEAVY_EVENTS,
            ),
            input_buffer_ms: 220,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: Some(MsTimingWindow::closed(120, 540)),
            branch_window: Some(MsTimingWindow::closed(120, 540)),
            chain_rule: None,
        },
        TechniqueId::PenguinHeavy2 => TechniqueDefinition {
            id,
            action: FighterAction::HeavyAttack2,
            button: TechniqueButton::B,
            status: TechniqueStatus::Grounded,
            script: script(
                "penguin_sled_scoop.sc",
                Some(680),
                Some(820),
                1120,
                &PENGUIN_HEAVY2_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: Some(TechniqueChainRule {
                previous: PrevTechExpr {
                    any_of: PENGUIN_X_STEP_PREV,
                    command: Some(TechniqueButton::B),
                    conditions: X_CHAIN_CONDITIONS,
                },
                window: MsTimingWindow::closed(120, 540),
                same_button_required: true,
            }),
        },
        TechniqueId::PenguinDashAttack => TechniqueDefinition {
            id,
            action: FighterAction::DashAttack,
            button: TechniqueButton::Dash,
            status: TechniqueStatus::Grounded,
            script: script(
                "penguin_dash_snowflake_shot.sc",
                None,
                None,
                160,
                &PENGUIN_DASH_ATTACK_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::PenguinDashHeavy => TechniqueDefinition {
            id,
            action: FighterAction::DashAttack,
            button: TechniqueButton::B,
            status: TechniqueStatus::Grounded,
            script: script(
                "penguin_snow_slope_slide.sc",
                None,
                None,
                180,
                &PENGUIN_DASH_HEAVY_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::PenguinJumpAttack => TechniqueDefinition {
            id,
            action: FighterAction::JumpAttack,
            button: TechniqueButton::A,
            status: TechniqueStatus::Airborne,
            script: script(
                "penguin_air_snowflake_shot.sc",
                None,
                None,
                160,
                &PENGUIN_JUMP_ATTACK_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::PenguinJumpHeavy => TechniqueDefinition {
            id,
            action: FighterAction::JumpHeavyAttack,
            button: TechniqueButton::B,
            status: TechniqueStatus::Airborne,
            script: script(
                "penguin_snowflake_warp.sc",
                None,
                None,
                140,
                &PENGUIN_JUMP_HEAVY_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::PenguinUltimateStartup => TechniqueDefinition {
            id,
            action: FighterAction::UltimateStartup,
            button: TechniqueButton::Ultimate,
            status: TechniqueStatus::Grounded,
            script: script(
                "penguin_ice_field_ultimate.sc",
                None,
                None,
                620,
                &PENGUIN_ULTIMATE_ICE_FIELD_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: ULTIMATE_STAMINA_COST,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::PenguinUltimateRush => TechniqueDefinition {
            id,
            action: FighterAction::UltimateRush,
            button: TechniqueButton::Ultimate,
            status: TechniqueStatus::Grounded,
            script: script(
                "penguin_snow_slope_ultimate_rush.sc",
                None,
                None,
                680,
                &PENGUIN_ULTIMATE_SLOPE_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::ChickLight1 => TechniqueDefinition {
            id,
            action: FighterAction::LightAttack1,
            button: TechniqueButton::A,
            status: TechniqueStatus::Grounded,
            script: script(
                "chick_orbit_egg_launch.sc",
                Some(170),
                None,
                260,
                &CHICK_LIGHT1_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::ChickLight2 => TechniqueDefinition {
            id,
            action: FighterAction::LightAttack2,
            button: TechniqueButton::A,
            status: TechniqueStatus::Grounded,
            script: script(
                "chick_sunny_flip.sc",
                Some(315),
                Some(350),
                460,
                &CHICK_LIGHT2_EVENTS,
            ),
            input_buffer_ms: 180,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: Some(MsTimingWindow::closed(130, 350)),
            branch_window: Some(MsTimingWindow::closed(130, 350)),
            chain_rule: Some(TechniqueChainRule {
                previous: PrevTechExpr {
                    any_of: CHICK_A_S_PREV,
                    command: Some(TechniqueButton::A),
                    conditions: A_CHAIN_CONDITIONS,
                },
                window: MsTimingWindow::closed(90, 250),
                same_button_required: true,
            }),
        },
        TechniqueId::ChickComboFinisher => TechniqueDefinition {
            id,
            action: FighterAction::ComboFinisher,
            button: TechniqueButton::A,
            status: TechniqueStatus::Grounded,
            script: script(
                "chick_shell_scramble.sc",
                Some(430),
                Some(610),
                760,
                &CHICK_COMBO_FINISHER_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: Some(TechniqueChainRule {
                previous: PrevTechExpr {
                    any_of: CHICK_A_SS_PREV,
                    command: Some(TechniqueButton::A),
                    conditions: A_FINISHER_CONDITIONS,
                },
                window: MsTimingWindow::closed(130, 350),
                same_button_required: true,
            }),
        },
        TechniqueId::ChickHeavy => TechniqueDefinition {
            id,
            action: FighterAction::HeavyAttack,
            button: TechniqueButton::B,
            status: TechniqueStatus::Grounded,
            script: script(
                "chick_orbit_egg.sc",
                Some(420),
                Some(520),
                720,
                &CHICK_HEAVY_EVENTS,
            ),
            input_buffer_ms: 220,
            stamina_cost: CHICK_X_STAMINA_COST,
            movement_lock: MovementLock::Locked,
            cancel_window: Some(MsTimingWindow::closed(120, 520)),
            branch_window: Some(MsTimingWindow::closed(120, 520)),
            chain_rule: None,
        },
        TechniqueId::ChickHeavy2 => TechniqueDefinition {
            id,
            action: FighterAction::HeavyAttack2,
            button: TechniqueButton::B,
            status: TechniqueStatus::Grounded,
            script: script(
                "chick_eggplant_impostor.sc",
                Some(610),
                None,
                900,
                &CHICK_HEAVY2_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: Some(TechniqueChainRule {
                previous: PrevTechExpr {
                    any_of: CHICK_X_STEP_PREV,
                    command: Some(TechniqueButton::B),
                    conditions: X_CHAIN_CONDITIONS,
                },
                window: MsTimingWindow::closed(120, 520),
                same_button_required: true,
            }),
        },
        TechniqueId::ChickDashAttack => TechniqueDefinition {
            id,
            action: FighterAction::DashAttack,
            button: TechniqueButton::Dash,
            status: TechniqueStatus::Grounded,
            script: script(
                "chick_dash_backstep_c.sc",
                Some(220),
                None,
                CHICK_DASH_BACKSTEP_RECOVER_MS,
                &CHICK_DASH_ATTACK_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::ChickDashHeavy => TechniqueDefinition {
            id,
            action: FighterAction::DashAttack,
            button: TechniqueButton::Dash,
            status: TechniqueStatus::Grounded,
            script: script(
                "chick_dash_backstep_x.sc",
                Some(220),
                None,
                CHICK_DASH_BACKSTEP_RECOVER_MS,
                &CHICK_DASH_HEAVY_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::ChickJumpAttack => TechniqueDefinition {
            id,
            action: FighterAction::JumpAttack,
            button: TechniqueButton::A,
            status: TechniqueStatus::Airborne,
            script: script(
                "chick_updraft_glide.sc",
                Some(430),
                None,
                620,
                &CHICK_JUMP_ATTACK_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::ChickJumpHeavy => TechniqueDefinition {
            id,
            action: FighterAction::JumpHeavyAttack,
            button: TechniqueButton::B,
            status: TechniqueStatus::Airborne,
            script: script(
                "chick_fresh_egg_ride.sc",
                Some(260),
                None,
                560,
                &CHICK_JUMP_HEAVY_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::ChickUltimateStartup => TechniqueDefinition {
            id,
            action: FighterAction::UltimateStartup,
            button: TechniqueButton::Ultimate,
            status: TechniqueStatus::Grounded,
            script: script(
                "chick_egg_burst.sc",
                Some(360),
                None,
                560,
                &CHICK_ULTIMATE_STARTUP_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: ULTIMATE_STAMINA_COST,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::PandaLight1 => TechniqueDefinition {
            id,
            action: FighterAction::LightAttack1,
            button: TechniqueButton::A,
            status: TechniqueStatus::Grounded,
            script: script(
                "panda_palm_1.sc",
                Some(680),
                Some(720),
                860,
                &PANDA_LIGHT1_EVENTS,
            ),
            input_buffer_ms: 160,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: Some(MsTimingWindow::closed(240, 720)),
            branch_window: Some(MsTimingWindow::closed(240, 720)),
            chain_rule: None,
        },
        TechniqueId::PandaLight2 => TechniqueDefinition {
            id,
            action: FighterAction::LightAttack2,
            button: TechniqueButton::A,
            status: TechniqueStatus::Grounded,
            script: script(
                "panda_palm_2.sc",
                Some(720),
                Some(760),
                920,
                &PANDA_LIGHT2_EVENTS,
            ),
            input_buffer_ms: 160,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: Some(MsTimingWindow::closed(260, 760)),
            branch_window: Some(MsTimingWindow::closed(260, 760)),
            chain_rule: Some(TechniqueChainRule {
                previous: PrevTechExpr {
                    any_of: PANDA_A_S_PREV,
                    command: Some(TechniqueButton::A),
                    conditions: A_CHAIN_CONDITIONS,
                },
                window: MsTimingWindow::closed(240, 720),
                same_button_required: true,
            }),
        },
        TechniqueId::PandaComboFinisher => TechniqueDefinition {
            id,
            action: FighterAction::ComboFinisher,
            button: TechniqueButton::A,
            status: TechniqueStatus::Grounded,
            script: script(
                "panda_body_drop.sc",
                Some(760),
                None,
                1120,
                &PANDA_COMBO_FINISHER_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: Some(TechniqueChainRule {
                previous: PrevTechExpr {
                    any_of: PANDA_A_SS_PREV,
                    command: Some(TechniqueButton::A),
                    conditions: A_FINISHER_CONDITIONS,
                },
                window: MsTimingWindow::closed(260, 760),
                same_button_required: true,
            }),
        },
        TechniqueId::PandaHeavy => TechniqueDefinition {
            id,
            action: FighterAction::HeavyAttack,
            button: TechniqueButton::B,
            status: TechniqueStatus::Grounded,
            script: script(
                "panda_weight_shift.sc",
                Some(520),
                Some(560),
                780,
                &PANDA_HEAVY_EVENTS,
            ),
            input_buffer_ms: 180,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: Some(MsTimingWindow::closed(140, 560)),
            branch_window: Some(MsTimingWindow::closed(140, 560)),
            chain_rule: None,
        },
        TechniqueId::PandaHeavy2 => TechniqueDefinition {
            id,
            action: FighterAction::HeavyAttack2,
            button: TechniqueButton::B,
            status: TechniqueStatus::Grounded,
            script: script(
                "panda_rising_scoop.sc",
                Some(720),
                Some(860),
                1180,
                &PANDA_HEAVY2_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: Some(TechniqueChainRule {
                previous: PrevTechExpr {
                    any_of: PANDA_X_STEP_PREV,
                    command: Some(TechniqueButton::B),
                    conditions: X_CHAIN_CONDITIONS,
                },
                window: MsTimingWindow::closed(140, 560),
                same_button_required: true,
            }),
        },
        TechniqueId::PandaJumpAttack => TechniqueDefinition {
            id,
            action: FighterAction::JumpAttack,
            button: TechniqueButton::A,
            status: TechniqueStatus::Airborne,
            script: script(
                "panda_air_drop.sc",
                None,
                None,
                680,
                &PANDA_JUMP_ATTACK_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::PandaJumpHeavy => TechniqueDefinition {
            id,
            action: FighterAction::JumpHeavyAttack,
            button: TechniqueButton::B,
            status: TechniqueStatus::Airborne,
            script: script(
                "panda_air_fish.sc",
                None,
                None,
                720,
                &PANDA_JUMP_HEAVY_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::PandaUltimateStartup => TechniqueDefinition {
            id,
            action: FighterAction::UltimateStartup,
            button: TechniqueButton::Ultimate,
            status: TechniqueStatus::Grounded,
            script: script(
                "panda_ultimate_startup.sc",
                Some(640),
                None,
                840,
                &PANDA_ULTIMATE_STARTUP_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: ULTIMATE_STAMINA_COST,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::PandaUltimateRush => TechniqueDefinition {
            id,
            action: FighterAction::UltimateRush,
            button: TechniqueButton::Ultimate,
            status: TechniqueStatus::Grounded,
            script: script(
                "panda_ultimate_rush.sc",
                Some(1240),
                None,
                1560,
                &PANDA_ULTIMATE_RUSH_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::CatUltimateStartup => TechniqueDefinition {
            id,
            action: FighterAction::UltimateStartup,
            button: TechniqueButton::Ultimate,
            status: TechniqueStatus::Grounded,
            script: script(
                "ultimate_startup.sc",
                Some(520),
                None,
                780,
                &ULTIMATE_STARTUP_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: ULTIMATE_STAMINA_COST,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::CatUltimateRush => TechniqueDefinition {
            id,
            action: FighterAction::UltimateRush,
            button: TechniqueButton::Ultimate,
            status: TechniqueStatus::Grounded,
            script: script(
                "ultimate_rush.sc",
                Some(960),
                None,
                1260,
                &ULTIMATE_RUSH_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::Grab => TechniqueDefinition {
            id,
            action: FighterAction::GrabStartup,
            button: TechniqueButton::Grab,
            status: TechniqueStatus::Grounded,
            script: script("grab.sc", None, None, 640, &GRAB_EVENTS),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::Throw => simple_technique(
            id,
            FighterAction::Throwing,
            TechniqueButton::Grab,
            320,
            MovementLock::Locked,
        ),
        TechniqueId::CatDashAttack
        | TechniqueId::PigDashAttack
        | TechniqueId::DogDashAttack
        | TechniqueId::FoxDashAttack
        | TechniqueId::PandaDashAttack
        | TechniqueId::BeeDashAttack => TechniqueDefinition {
            id,
            action: FighterAction::DashAttack,
            button: TechniqueButton::Dash,
            status: TechniqueStatus::Grounded,
            script: script(
                "dash_attack.sc",
                Some(360),
                Some(500),
                580,
                &DASH_ATTACK_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::CatJumpAttack => TechniqueDefinition {
            id,
            action: FighterAction::JumpAttack,
            button: TechniqueButton::A,
            status: TechniqueStatus::Airborne,
            script: script("jump_attack.sc", None, None, 460, &JUMP_ATTACK_EVENTS),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::CatJumpHeavy => TechniqueDefinition {
            id,
            action: FighterAction::JumpHeavyAttack,
            button: TechniqueButton::B,
            status: TechniqueStatus::Airborne,
            script: script("jump_x_fish.sc", None, None, 560, &JUMP_HEAVY_EVENTS),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::GuardCounter => TechniqueDefinition {
            id,
            action: FighterAction::GuardCounter,
            button: TechniqueButton::A,
            status: TechniqueStatus::Grounded,
            script: script("guard_counter.sc", None, None, 430, &GUARD_COUNTER_EVENTS),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::SpecialCast => TechniqueDefinition {
            id,
            action: FighterAction::SpecialCast,
            button: TechniqueButton::Special,
            status: TechniqueStatus::Grounded,
            script: script(
                "special_cast.sc",
                Some(260),
                None,
                360,
                &SPECIAL_CAST_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::ItemPickup => TechniqueDefinition {
            id,
            action: FighterAction::ItemPickup,
            button: TechniqueButton::Item,
            status: TechniqueStatus::Grounded,
            script: script("item_pickup.sc", Some(150), None, 240, &ITEM_PICKUP_EVENTS),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Grounded,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::ItemSwing => simple_technique(
            id,
            FighterAction::ItemSwing,
            TechniqueButton::Item,
            ((ITEM_SWING_STARTUP + ITEM_SWING_ACTIVE + ITEM_SWING_RECOVERY) * MS_PER_SECOND) as u32,
            MovementLock::Grounded,
        ),
        TechniqueId::ItemThrow => simple_technique(
            id,
            FighterAction::ItemThrow,
            TechniqueButton::Item,
            (ITEM_THROW_DURATION * MS_PER_SECOND) as u32,
            MovementLock::Grounded,
        ),
        TechniqueId::ItemDrop => TechniqueDefinition {
            id,
            action: FighterAction::ItemDrop,
            button: TechniqueButton::Item,
            status: TechniqueStatus::Grounded,
            script: script("item_drop.sc", Some(140), None, 220, &ITEM_DROP_EVENTS),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Grounded,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::GuardStep => TechniqueDefinition {
            id,
            action: FighterAction::GuardStep,
            button: TechniqueButton::AB,
            status: TechniqueStatus::Grounded,
            script: script("guard_step.sc", Some(130), None, 260, &GUARD_STEP_EVENTS),
            input_buffer_ms: 0,
            stamina_cost: GUARD_STEP_STAMINA_COST,
            movement_lock: MovementLock::Grounded,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::QuickStand => TechniqueDefinition {
            id,
            action: FighterAction::QuickStand,
            button: TechniqueButton::Jump,
            status: TechniqueStatus::Grounded,
            script: script("quick_stand.sc", Some(150), None, 240, &QUICK_STAND_EVENTS),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Locked,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::RecoveryRoll => TechniqueDefinition {
            id,
            action: FighterAction::RecoveryRoll,
            button: TechniqueButton::Dash,
            status: TechniqueStatus::Grounded,
            script: script(
                "recovery_roll.sc",
                Some(240),
                None,
                360,
                &RECOVERY_ROLL_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Grounded,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
        TechniqueId::LandingRecovery => TechniqueDefinition {
            id,
            action: FighterAction::LandingRecovery,
            button: TechniqueButton::Jump,
            status: TechniqueStatus::Grounded,
            script: script(
                "landing_recovery.sc",
                Some(135),
                None,
                220,
                &LANDING_RECOVERY_EVENTS,
            ),
            input_buffer_ms: 0,
            stamina_cost: 0.0,
            movement_lock: MovementLock::Grounded,
            cancel_window: None,
            branch_window: None,
            chain_rule: None,
        },
    })
}

#[allow(dead_code)]
pub fn technique_definition_for_style(
    action: FighterAction,
    style: FighterStyleKind,
) -> Option<TechniqueDefinition> {
    technique_definition_for_loadout(action, LoadoutContext::from_style(style))
}

pub fn technique_definition_for_loadout(
    action: FighterAction,
    loadout: LoadoutContext,
) -> Option<TechniqueDefinition> {
    technique_definition(action)
        .map(|definition| apply_loadout_technique_modifiers(definition, loadout))
}

fn apply_loadout_technique_modifiers(
    mut definition: TechniqueDefinition,
    loadout: LoadoutContext,
) -> TechniqueDefinition {
    let action = definition.action;

    if let Some(modifier) = loadout_technique_modifier(loadout, action) {
        scale_total_duration(&mut definition, modifier.duration_scale);
        if let Some((start, end)) = modifier.branch_window_secs {
            definition.branch_window = Some(MsTimingWindow::closed(
                (start * MS_PER_SECOND) as u32,
                (end * MS_PER_SECOND) as u32,
            ));
            definition.cancel_window = definition.branch_window;
        }
    }

    definition
}

pub fn technique_definition_for_loadout_in_catalog(
    action: FighterAction,
    loadout: LoadoutContext,
    catalog: &CharacterMoveCatalog,
) -> Option<TechniqueDefinition> {
    let id = catalog
        .ordered_techniques(loadout.character)
        .iter()
        .copied()
        .find(|id| {
            technique_definition_by_id(*id).is_some_and(|definition| definition.action == action)
        })?;
    technique_definition_for_loadout_id_in_catalog(id, loadout, catalog)
}

pub fn technique_definition_for_loadout_id_in_catalog(
    id: TechniqueId,
    loadout: LoadoutContext,
    catalog: &CharacterMoveCatalog,
) -> Option<TechniqueDefinition> {
    catalog
        .allows_technique(loadout.character, id)
        .then_some(())
        .and_then(|_| technique_definition_for_loadout_id(id, loadout))
}

fn authored_active_action_requires_technique_id(action: FighterAction) -> bool {
    matches!(
        action,
        FighterAction::LightAttack1
            | FighterAction::LightAttack2
            | FighterAction::ComboFinisher
            | FighterAction::HeavyAttack
            | FighterAction::HeavyAttack2
            | FighterAction::UltimateStartup
            | FighterAction::UltimateRush
            | FighterAction::DashAttack
            | FighterAction::JumpAttack
            | FighterAction::JumpHeavyAttack
    )
}

pub fn active_technique_definition_in_catalog(
    action: FighterAction,
    technique_id: Option<TechniqueId>,
    loadout: LoadoutContext,
    catalog: &CharacterMoveCatalog,
) -> Option<TechniqueDefinition> {
    if let Some(definition) = technique_id
        .and_then(|id| technique_definition_for_loadout_id_in_catalog(id, loadout, catalog))
        .filter(|definition| definition.action == action)
    {
        return Some(definition);
    }

    if authored_active_action_requires_technique_id(action) {
        return None;
    }

    technique_definition_for_loadout_in_catalog(action, loadout, catalog)
}

pub fn technique_slot_for_loadout(
    slot: CharacterMoveSlot,
    loadout: LoadoutContext,
    catalog: &CharacterMoveCatalog,
) -> Option<TechniqueDefinition> {
    let id = catalog.slot_technique(loadout.character, slot)?;
    technique_definition_for_loadout_id_in_catalog(id, loadout, catalog)
}

#[allow(dead_code)]
pub fn raw_technique_for_button(
    button: TechniqueButton,
    grounded: bool,
    style: FighterStyleKind,
) -> Option<TechniqueDefinition> {
    raw_technique_for_loadout(button, grounded, LoadoutContext::from_style(style))
}

pub fn raw_technique_for_loadout(
    button: TechniqueButton,
    grounded: bool,
    loadout: LoadoutContext,
) -> Option<TechniqueDefinition> {
    AUTHORED_TECHNIQUE_ORDER
        .iter()
        .filter_map(|id| technique_definition_for_loadout_id(*id, loadout))
        .find(|definition| {
            definition.chain_rule.is_none()
                && definition.button == button
                && definition.status.allows(grounded)
        })
}

pub fn raw_technique_for_loadout_in_catalog(
    button: TechniqueButton,
    grounded: bool,
    loadout: LoadoutContext,
    catalog: &CharacterMoveCatalog,
) -> Option<TechniqueDefinition> {
    catalog
        .ordered_techniques(loadout.character)
        .iter()
        .filter_map(|id| technique_definition_for_loadout_id_in_catalog(*id, loadout, catalog))
        .find(|definition| {
            definition.chain_rule.is_none()
                && definition.button == button
                && definition.status.allows(grounded)
        })
}

#[allow(dead_code)]
pub fn chained_technique_for_button(
    previous: Option<TechniqueId>,
    button: TechniqueButton,
    elapsed: f32,
    style: FighterStyleKind,
) -> Option<TechniqueDefinition> {
    chained_technique_for_context(TechniqueMatchContext {
        previous,
        button,
        elapsed,
        style,
        loadout: LoadoutContext::from_style(style),
        grounded: true,
        confirmed_hit: false,
        cancel_window_open: false,
        branch_window_open: true,
        current_action: FighterAction::Idle,
    })
}

pub fn chained_technique_for_context(
    context: TechniqueMatchContext,
) -> Option<TechniqueDefinition> {
    let previous = context.previous?;
    AUTHORED_TECHNIQUE_ORDER
        .iter()
        .filter_map(|id| technique_definition_for_loadout_id(*id, context.loadout))
        .find(|definition| {
            let Some(rule) = definition.chain_rule else {
                return false;
            };
            if rule.same_button_required
                && !same_button_cancel_gate_for_context(
                    previous,
                    context.button,
                    context.branch_window_open,
                    context.loadout,
                )
            {
                return false;
            }
            definition.button == context.button && rule.matches(context)
        })
}

pub fn chained_technique_for_context_in_catalog(
    context: TechniqueMatchContext,
    catalog: &CharacterMoveCatalog,
) -> Option<TechniqueDefinition> {
    let previous = context.previous?;
    catalog
        .ordered_techniques(context.loadout.character)
        .iter()
        .filter_map(|id| {
            technique_definition_for_loadout_id_in_catalog(*id, context.loadout, catalog)
        })
        .find(|definition| {
            let Some(rule) = definition.chain_rule else {
                return false;
            };
            if rule.same_button_required
                && !same_button_cancel_gate_for_context_in_catalog(
                    previous,
                    context.button,
                    context.branch_window_open,
                    context.loadout,
                    catalog,
                )
            {
                return false;
            }
            definition.button == context.button && rule.matches(context)
        })
}

#[allow(dead_code)]
pub fn same_button_cancel_gate(
    previous: TechniqueId,
    button: TechniqueButton,
    elapsed: f32,
    style: FighterStyleKind,
) -> bool {
    same_button_cancel_gate_for_loadout(
        previous,
        button,
        elapsed,
        LoadoutContext::from_style(style),
    )
}

pub fn same_button_cancel_gate_for_loadout(
    previous: TechniqueId,
    button: TechniqueButton,
    elapsed: f32,
    loadout: LoadoutContext,
) -> bool {
    let Some(previous_definition) = technique_definition_for_loadout_id(previous, loadout) else {
        return false;
    };
    previous_definition.button == button && previous_definition.branch_open(elapsed)
}

fn same_button_cancel_gate_for_context(
    previous: TechniqueId,
    button: TechniqueButton,
    branch_window_open: bool,
    loadout: LoadoutContext,
) -> bool {
    let Some(previous_definition) = technique_definition_for_loadout_id(previous, loadout) else {
        return false;
    };
    previous_definition.button == button && branch_window_open
}

#[allow(dead_code)]
pub fn same_button_cancel_gate_for_loadout_in_catalog(
    previous: TechniqueId,
    button: TechniqueButton,
    elapsed: f32,
    loadout: LoadoutContext,
    catalog: &CharacterMoveCatalog,
) -> bool {
    let Some(previous_definition) =
        technique_definition_for_loadout_id_in_catalog(previous, loadout, catalog)
    else {
        return false;
    };
    previous_definition.button == button && previous_definition.branch_open(elapsed)
}

fn same_button_cancel_gate_for_context_in_catalog(
    previous: TechniqueId,
    button: TechniqueButton,
    branch_window_open: bool,
    loadout: LoadoutContext,
    catalog: &CharacterMoveCatalog,
) -> bool {
    let Some(previous_definition) =
        technique_definition_for_loadout_id_in_catalog(previous, loadout, catalog)
    else {
        return false;
    };
    previous_definition.button == button && branch_window_open
}

#[allow(dead_code)]
pub fn technique_runtime(
    action: FighterAction,
    elapsed: f32,
    style: FighterStyleKind,
) -> TechniqueRuntime {
    technique_runtime_for_loadout(action, elapsed, LoadoutContext::from_style(style))
}

pub fn technique_runtime_for_loadout(
    action: FighterAction,
    elapsed: f32,
    loadout: LoadoutContext,
) -> TechniqueRuntime {
    let Some(definition) = technique_definition_for_loadout(action, loadout) else {
        return TechniqueRuntime {
            id: None,
            cancel_open: false,
            branch_open: false,
            next_tech_open: false,
            recovered: false,
        };
    };

    TechniqueRuntime {
        id: Some(definition.id),
        cancel_open: definition.cancel_open(elapsed),
        branch_open: definition.branch_open(elapsed),
        next_tech_open: definition.script.next_tech_open(elapsed),
        recovered: definition.script.recovered(elapsed),
    }
}

#[allow(dead_code)]
pub fn technique_runtime_for_loadout_in_catalog(
    action: FighterAction,
    elapsed: f32,
    loadout: LoadoutContext,
    catalog: &CharacterMoveCatalog,
) -> TechniqueRuntime {
    let Some(definition) = technique_definition_for_loadout_in_catalog(action, loadout, catalog)
    else {
        return TechniqueRuntime {
            id: None,
            cancel_open: false,
            branch_open: false,
            next_tech_open: false,
            recovered: false,
        };
    };

    TechniqueRuntime {
        id: Some(definition.id),
        cancel_open: definition.cancel_open(elapsed),
        branch_open: definition.branch_open(elapsed),
        next_tech_open: definition.script.next_tech_open(elapsed),
        recovered: definition.script.recovered(elapsed),
    }
}

#[allow(dead_code)]
fn technique_definition_for_style_id(
    id: TechniqueId,
    style: FighterStyleKind,
) -> Option<TechniqueDefinition> {
    technique_definition_for_loadout_id(id, LoadoutContext::from_style(style))
}

fn technique_definition_for_loadout_id(
    id: TechniqueId,
    loadout: LoadoutContext,
) -> Option<TechniqueDefinition> {
    let definition = technique_definition_by_id(id)?;
    Some(apply_loadout_technique_modifiers(definition, loadout))
}

fn scale_total_duration(definition: &mut TechniqueDefinition, scale: f32) {
    definition.script.recover_ms = ((definition.script.recover_ms as f32) * scale) as u32;
}

fn script(
    id: &'static str,
    animation_recovery_ms: Option<u32>,
    next_tech_ms: Option<u32>,
    recover_ms: u32,
    events: &'static [MoveTimelineEvent],
) -> MoveScriptDef {
    MoveScriptDef {
        id,
        animation_recovery_ms,
        next_tech_ms,
        recover_ms,
        events,
    }
}

fn simple_technique(
    id: TechniqueId,
    action: FighterAction,
    button: TechniqueButton,
    duration_ms: u32,
    movement_lock: MovementLock,
) -> TechniqueDefinition {
    TechniqueDefinition {
        id,
        action,
        button,
        status: TechniqueStatus::Any,
        script: script(id.label(), None, None, duration_ms, &EMPTY_EVENTS),
        input_buffer_ms: 0,
        stamina_cost: 0.0,
        movement_lock,
        cancel_window: None,
        branch_window: None,
        chain_rule: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timeline_timing_signature(
        technique: &TechniqueDefinition,
        feel: Option<&crate::feel::CombatFeelTuning>,
    ) -> Vec<(u32, &'static str)> {
        technique
            .script
            .events
            .iter()
            .enumerate()
            .map(|(index, event)| {
                let at_ms = feel
                    .map(|feel| feel.timeline_event_at_ms(technique, index, event))
                    .unwrap_or(event.at_ms);
                (at_ms, timeline_event_kind_key(event))
            })
            .collect()
    }

    fn timeline_event_kind_key(event: &MoveTimelineEvent) -> &'static str {
        match event.kind {
            MoveTimelineEventKind::Attack(_) => "attack",
            MoveTimelineEventKind::ChargedAttack { .. } => "charged_attack",
            MoveTimelineEventKind::SpawnBeeSkill(_) => "bee_skill",
            MoveTimelineEventKind::SpawnPenguinSkill(_) => "penguin_skill",
            MoveTimelineEventKind::SpawnChickSkill(_) => "chick_skill",
            MoveTimelineEventKind::Feedback(phase, _) => match phase {
                FeedbackPhase::Startup => "startup_feedback",
                FeedbackPhase::PreHit => "prehit_feedback",
                FeedbackPhase::Impact => "impact_feedback",
                FeedbackPhase::Aftermath => "aftermath_feedback",
            },
            MoveTimelineEventKind::Motion { .. } => "motion",
            MoveTimelineEventKind::NextTech => "next_tech",
            MoveTimelineEventKind::Recover => "recover",
            MoveTimelineEventKind::Stop => "stop",
        }
    }

    #[test]
    fn light_string_keeps_one_contact_per_button_press() {
        let technique = technique_definition_by_id(TechniqueId::CatLight1).unwrap();
        let attacks: Vec<(u32, AttackPayloadId)> = technique
            .script
            .events
            .iter()
            .filter_map(|event| match event.kind {
                MoveTimelineEventKind::Attack(payload) => Some((event.at_ms, payload)),
                _ => None,
            })
            .collect();
        assert_eq!(technique.id, TechniqueId::CatLight1);
        assert_eq!(attacks, vec![(440, AttackPayloadId::AsBeat1)]);
        assert_eq!(
            technique.branch_window.unwrap(),
            MsTimingWindow::closed(200, 600)
        );
        assert_eq!(technique.script.animation_recovery_ms, Some(686));
        assert_eq!(technique.script.next_tech_ms, Some(720));
        assert_eq!(technique.script.recover_ms, 800);

        let followup = technique_definition_by_id(TechniqueId::CatLight2).unwrap();
        let followup_attacks: Vec<AttackPayloadId> = followup
            .script
            .events
            .iter()
            .filter_map(|event| match event.kind {
                MoveTimelineEventKind::Attack(payload) => Some(payload),
                _ => None,
            })
            .collect();
        assert_eq!(followup_attacks, vec![AttackPayloadId::AssBeat1]);
        assert_eq!(
            attack_payload_definition(AttackPayloadId::AssBeat1).shape_id,
            AttackShapeId::CompactSlashFollow
        );
        assert_ne!(
            attack_payload_definition(AttackPayloadId::AssBeat1).shape_id,
            attack_payload_definition(AttackPayloadId::AsBeat1).shape_id
        );
        let first_shape = attack_shape_definition(AttackShapeId::CompactSlashLead);
        let second_shape = attack_shape_definition(AttackShapeId::CompactSlashFollow);
        assert_eq!(first_shape.range, second_shape.range);
        assert_eq!(first_shape.radius, second_shape.radius);
        assert!(first_shape.range > FIGHTER_RADIUS * 1.45);
        assert!(first_shape.radius < LIGHT_RADIUS * 0.25);

        let finisher = technique_definition_by_id(TechniqueId::CatComboFinisher).unwrap();
        assert_eq!(
            finisher
                .script
                .events
                .iter()
                .filter(|event| matches!(event.kind, MoveTimelineEventKind::Attack(_)))
                .count(),
            1
        );
    }

    #[test]
    fn a_ss_is_ordered_chain_only_followup() {
        assert_eq!(
            raw_technique_for_button(TechniqueButton::A, true, FighterStyleKind::Anchor)
                .unwrap()
                .id,
            TechniqueId::CatLight1
        );
        assert_eq!(
            chained_technique_for_button(
                Some(TechniqueId::CatLight1),
                TechniqueButton::A,
                0.2,
                FighterStyleKind::Anchor,
            )
            .unwrap()
            .id,
            TechniqueId::CatLight2
        );
        assert!(
            chained_technique_for_button(
                Some(TechniqueId::CatLight1),
                TechniqueButton::B,
                0.3,
                FighterStyleKind::Anchor,
            )
            .is_none()
        );
        assert!(
            chained_technique_for_button(
                Some(TechniqueId::CatLight1),
                TechniqueButton::A,
                0.61,
                FighterStyleKind::Anchor,
            )
            .is_none()
        );
    }

    #[test]
    fn catalog_lookup_does_not_fallback_to_cat_moves() {
        let catalog =
            CharacterMoveCatalog::from_file(crate::characters::CharacterMoveCatalogFile {
                characters: vec![
                    crate::characters::CharacterProfileDef {
                        kind: CharacterKind::Cat,
                        label: "Cat".to_string(),
                        scene: "characters/kenney_cube_pets/animal-cat.glb".to_string(),
                        move_set: "cat_light".to_string(),
                        body: crate::characters::CharacterBodyDef::default(),
                    },
                    crate::characters::CharacterProfileDef {
                        kind: CharacterKind::Pig,
                        label: "Pig".to_string(),
                        scene: "characters/kenney_cube_pets/animal-pig.glb".to_string(),
                        move_set: "cat_light".to_string(),
                        body: crate::characters::pig_body_profile(),
                    },
                    crate::characters::CharacterProfileDef {
                        kind: CharacterKind::Dog,
                        label: "Dog".to_string(),
                        scene: "characters/kenney_cube_pets/animal-dog.glb".to_string(),
                        move_set: "dog_guard".to_string(),
                        body: crate::characters::CharacterBodyDef::default(),
                    },
                    crate::characters::CharacterProfileDef {
                        kind: CharacterKind::Fox,
                        label: "Fox".to_string(),
                        scene: "characters/kenney_cube_pets/animal-fox.glb".to_string(),
                        move_set: "cat_light".to_string(),
                        body: crate::characters::CharacterBodyDef::default(),
                    },
                    crate::characters::CharacterProfileDef {
                        kind: CharacterKind::Panda,
                        label: "Panda".to_string(),
                        scene: "characters/kenney_cube_pets/animal-panda.glb".to_string(),
                        move_set: "cat_light".to_string(),
                        body: crate::characters::CharacterBodyDef::default(),
                    },
                    crate::characters::CharacterProfileDef {
                        kind: CharacterKind::Bee,
                        label: "Bee".to_string(),
                        scene: "characters/kenney_cube_pets/animal-bee.glb".to_string(),
                        move_set: "cat_light".to_string(),
                        body: crate::characters::bee_body_profile(),
                    },
                    crate::characters::CharacterProfileDef {
                        kind: CharacterKind::Penguin,
                        label: "Penguin".to_string(),
                        scene: "characters/kenney_cube_pets/animal-penguin.glb".to_string(),
                        move_set: "cat_light".to_string(),
                        body: crate::characters::penguin_body_profile(),
                    },
                    crate::characters::CharacterProfileDef {
                        kind: CharacterKind::Chick,
                        label: "Chick".to_string(),
                        scene: "characters/kenney_cube_pets/animal-chick.glb".to_string(),
                        move_set: "cat_light".to_string(),
                        body: crate::characters::chick_body_profile(),
                    },
                ],
                move_sets: vec![
                    crate::characters::CharacterMoveSetDef {
                        id: "cat_light".to_string(),
                        order: vec![TechniqueId::CatLight1],
                        slots: Vec::new(),
                    },
                    crate::characters::CharacterMoveSetDef {
                        id: "dog_guard".to_string(),
                        order: vec![TechniqueId::GuardStep],
                        slots: Vec::new(),
                    },
                ],
            });
        let cat = LoadoutContext::for_character(
            CharacterKind::Cat,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let dog = LoadoutContext::for_character(
            CharacterKind::Dog,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let pig = LoadoutContext::for_character(
            CharacterKind::Pig,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let bee = LoadoutContext::for_character(
            CharacterKind::Bee,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let penguin = LoadoutContext::for_character(
            CharacterKind::Penguin,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let chick = LoadoutContext::for_character(
            CharacterKind::Chick,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );

        assert_eq!(
            raw_technique_for_loadout_in_catalog(TechniqueButton::A, true, cat, &catalog)
                .unwrap()
                .id,
            TechniqueId::CatLight1
        );
        assert!(
            raw_technique_for_loadout_in_catalog(TechniqueButton::A, true, pig, &catalog).is_none()
        );
        assert!(
            chained_technique_for_context_in_catalog(
                TechniqueMatchContext {
                    previous: Some(TechniqueId::CatLight1),
                    button: TechniqueButton::A,
                    elapsed: 0.25,
                    style: FighterStyleKind::Anchor,
                    loadout: pig,
                    grounded: true,
                    confirmed_hit: true,
                    cancel_window_open: true,
                    branch_window_open: true,
                    current_action: FighterAction::LightAttack1,
                },
                &catalog,
            )
            .is_none()
        );
        assert!(
            raw_technique_for_loadout_in_catalog(TechniqueButton::A, true, dog, &catalog).is_none()
        );
        assert!(
            raw_technique_for_loadout_in_catalog(TechniqueButton::A, true, bee, &catalog).is_none()
        );
        assert!(
            raw_technique_for_loadout_in_catalog(TechniqueButton::A, true, penguin, &catalog)
                .is_none()
        );
        assert!(
            raw_technique_for_loadout_in_catalog(TechniqueButton::A, true, chick, &catalog)
                .is_none()
        );
    }

    #[test]
    fn imported_character_move_sets_select_distinct_grounded_routes() {
        let catalog = CharacterMoveCatalog::default();
        let dog = LoadoutContext::for_character(
            CharacterKind::Dog,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let pig = LoadoutContext::for_character(
            CharacterKind::Pig,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let fox = LoadoutContext::for_character(
            CharacterKind::Fox,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let panda = LoadoutContext::for_character(
            CharacterKind::Panda,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let bee = LoadoutContext::for_character(
            CharacterKind::Bee,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let penguin = LoadoutContext::for_character(
            CharacterKind::Penguin,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let chick = LoadoutContext::for_character(
            CharacterKind::Chick,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );

        assert_eq!(
            raw_technique_for_loadout_in_catalog(TechniqueButton::A, true, dog, &catalog)
                .unwrap()
                .id,
            TechniqueId::DogLight1
        );
        assert_eq!(
            raw_technique_for_loadout_in_catalog(TechniqueButton::A, true, pig, &catalog)
                .unwrap()
                .id,
            TechniqueId::PigLight1
        );
        assert_eq!(
            raw_technique_for_loadout_in_catalog(TechniqueButton::B, true, fox, &catalog)
                .unwrap()
                .id,
            TechniqueId::FoxHeavy
        );
        assert_eq!(
            raw_technique_for_loadout_in_catalog(TechniqueButton::B, true, panda, &catalog)
                .unwrap()
                .id,
            TechniqueId::PandaHeavy
        );
        assert_eq!(
            raw_technique_for_loadout_in_catalog(TechniqueButton::A, true, bee, &catalog)
                .unwrap()
                .id,
            TechniqueId::BeeLight1
        );
        assert_eq!(
            raw_technique_for_loadout_in_catalog(TechniqueButton::B, true, bee, &catalog)
                .unwrap()
                .id,
            TechniqueId::BeeHeavy2
        );
        assert_eq!(
            raw_technique_for_loadout_in_catalog(TechniqueButton::A, true, penguin, &catalog)
                .unwrap()
                .id,
            TechniqueId::PenguinLight1
        );
        assert_eq!(
            raw_technique_for_loadout_in_catalog(TechniqueButton::B, true, penguin, &catalog)
                .unwrap()
                .id,
            TechniqueId::PenguinHeavy
        );
        assert_eq!(
            raw_technique_for_loadout_in_catalog(TechniqueButton::A, true, chick, &catalog)
                .unwrap()
                .id,
            TechniqueId::ChickLight1
        );
        assert_eq!(
            raw_technique_for_loadout_in_catalog(TechniqueButton::B, true, chick, &catalog)
                .unwrap()
                .id,
            TechniqueId::ChickHeavy
        );

        let dog_followup = chained_technique_for_context_in_catalog(
            TechniqueMatchContext {
                previous: Some(TechniqueId::DogLight1),
                button: TechniqueButton::A,
                elapsed: 0.25,
                style: FighterStyleKind::Anchor,
                loadout: dog,
                grounded: true,
                confirmed_hit: true,
                cancel_window_open: true,
                branch_window_open: true,
                current_action: FighterAction::LightAttack1,
            },
            &catalog,
        )
        .unwrap();

        assert_eq!(dog_followup.id, TechniqueId::DogLight2);
        let pig_followup = chained_technique_for_context_in_catalog(
            TechniqueMatchContext {
                previous: Some(TechniqueId::PigLight1),
                button: TechniqueButton::A,
                elapsed: 0.34,
                style: FighterStyleKind::Anchor,
                loadout: pig,
                grounded: true,
                confirmed_hit: true,
                cancel_window_open: true,
                branch_window_open: true,
                current_action: FighterAction::LightAttack1,
            },
            &catalog,
        )
        .unwrap();

        assert_eq!(pig_followup.id, TechniqueId::PigLight2);
        let penguin_followup = chained_technique_for_context_in_catalog(
            TechniqueMatchContext {
                previous: Some(TechniqueId::PenguinLight1),
                button: TechniqueButton::A,
                elapsed: 0.145,
                style: FighterStyleKind::Anchor,
                loadout: penguin,
                grounded: true,
                confirmed_hit: true,
                cancel_window_open: true,
                branch_window_open: true,
                current_action: FighterAction::LightAttack1,
            },
            &catalog,
        )
        .unwrap();

        assert_eq!(penguin_followup.id, TechniqueId::PenguinLight2);
        let chick_followup = chained_technique_for_context_in_catalog(
            TechniqueMatchContext {
                previous: Some(TechniqueId::ChickLight1),
                button: TechniqueButton::A,
                elapsed: 0.16,
                style: FighterStyleKind::Anchor,
                loadout: chick,
                grounded: true,
                confirmed_hit: true,
                cancel_window_open: true,
                branch_window_open: true,
                current_action: FighterAction::LightAttack1,
            },
            &catalog,
        )
        .unwrap();

        assert_eq!(chick_followup.id, TechniqueId::ChickLight2);
        let chick_finisher = chained_technique_for_context_in_catalog(
            TechniqueMatchContext {
                previous: Some(TechniqueId::ChickLight2),
                button: TechniqueButton::A,
                elapsed: 0.24,
                style: FighterStyleKind::Anchor,
                loadout: chick,
                grounded: true,
                confirmed_hit: true,
                cancel_window_open: true,
                branch_window_open: true,
                current_action: FighterAction::LightAttack2,
            },
            &catalog,
        )
        .unwrap();

        assert_eq!(chick_finisher.id, TechniqueId::ChickComboFinisher);
        assert!(
            chained_technique_for_context_in_catalog(
                TechniqueMatchContext {
                    previous: Some(TechniqueId::BeeLight1),
                    button: TechniqueButton::A,
                    elapsed: 0.2,
                    style: FighterStyleKind::Anchor,
                    loadout: bee,
                    grounded: true,
                    confirmed_hit: true,
                    cancel_window_open: true,
                    branch_window_open: true,
                    current_action: FighterAction::LightAttack1,
                },
                &catalog,
            )
            .is_none()
        );
        assert!(
            technique_definition_for_loadout_in_catalog(FighterAction::HeavyAttack, fox, &catalog)
                .unwrap()
                .duration()
                < technique_definition_for_loadout_in_catalog(
                    FighterAction::HeavyAttack,
                    panda,
                    &catalog
                )
                .unwrap()
                .duration()
        );
    }

    #[test]
    fn character_catalog_slots_cover_air_and_ultimate_routes() {
        let catalog = CharacterMoveCatalog::default();
        let dog = LoadoutContext::for_character(
            CharacterKind::Dog,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let pig = LoadoutContext::for_character(
            CharacterKind::Pig,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let fox = LoadoutContext::for_character(
            CharacterKind::Fox,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let panda = LoadoutContext::for_character(
            CharacterKind::Panda,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let bee = LoadoutContext::for_character(
            CharacterKind::Bee,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let penguin = LoadoutContext::for_character(
            CharacterKind::Penguin,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let chick = LoadoutContext::for_character(
            CharacterKind::Chick,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );

        assert_eq!(
            technique_slot_for_loadout(CharacterMoveSlot::JumpLight, dog, &catalog)
                .unwrap()
                .id,
            TechniqueId::DogJumpAttack
        );
        assert_eq!(
            technique_slot_for_loadout(CharacterMoveSlot::JumpHeavy, pig, &catalog)
                .unwrap()
                .id,
            TechniqueId::PigJumpHeavy
        );
        assert_eq!(
            technique_slot_for_loadout(CharacterMoveSlot::JumpHeavy, fox, &catalog)
                .unwrap()
                .id,
            TechniqueId::FoxJumpHeavy
        );
        assert_eq!(
            raw_technique_for_loadout_in_catalog(TechniqueButton::Ultimate, true, panda, &catalog)
                .unwrap()
                .id,
            TechniqueId::PandaUltimateStartup
        );
        assert_eq!(
            technique_slot_for_loadout(CharacterMoveSlot::UltimateRush, panda, &catalog)
                .unwrap()
                .id,
            TechniqueId::PandaUltimateRush
        );
        assert_eq!(
            technique_slot_for_loadout(CharacterMoveSlot::JumpLight, bee, &catalog)
                .unwrap()
                .id,
            TechniqueId::BeeJumpAttack
        );
        assert_eq!(
            technique_slot_for_loadout(CharacterMoveSlot::JumpHeavy, bee, &catalog)
                .unwrap()
                .id,
            TechniqueId::BeeJumpHeavy
        );
        assert_eq!(
            raw_technique_for_loadout_in_catalog(TechniqueButton::Ultimate, true, bee, &catalog)
                .unwrap()
                .id,
            TechniqueId::BeeUltimateStartup
        );
        assert!(
            technique_slot_for_loadout(CharacterMoveSlot::UltimateRush, bee, &catalog).is_none()
        );
        assert_eq!(
            technique_slot_for_loadout(CharacterMoveSlot::JumpLight, penguin, &catalog)
                .unwrap()
                .id,
            TechniqueId::PenguinJumpAttack
        );
        assert_eq!(
            technique_slot_for_loadout(CharacterMoveSlot::JumpHeavy, penguin, &catalog)
                .unwrap()
                .id,
            TechniqueId::PenguinJumpHeavy
        );
        assert_eq!(
            raw_technique_for_loadout_in_catalog(
                TechniqueButton::Ultimate,
                true,
                penguin,
                &catalog
            )
            .unwrap()
            .id,
            TechniqueId::PenguinUltimateStartup
        );
        assert_eq!(
            technique_slot_for_loadout(CharacterMoveSlot::DashLight, chick, &catalog)
                .unwrap()
                .id,
            TechniqueId::ChickDashAttack
        );
        assert_eq!(
            technique_slot_for_loadout(CharacterMoveSlot::DashHeavy, chick, &catalog)
                .unwrap()
                .id,
            TechniqueId::ChickDashHeavy
        );
        assert_eq!(
            technique_slot_for_loadout(CharacterMoveSlot::JumpLight, chick, &catalog)
                .unwrap()
                .id,
            TechniqueId::ChickJumpAttack
        );
        assert_eq!(
            technique_slot_for_loadout(CharacterMoveSlot::JumpHeavy, chick, &catalog)
                .unwrap()
                .id,
            TechniqueId::ChickJumpHeavy
        );
        assert_eq!(
            raw_technique_for_loadout_in_catalog(TechniqueButton::Ultimate, true, chick, &catalog)
                .unwrap()
                .id,
            TechniqueId::ChickUltimateStartup
        );
        assert!(
            technique_slot_for_loadout(CharacterMoveSlot::UltimateRush, chick, &catalog).is_none()
        );
        assert!(payload_is_ultimate_catch(AttackPayloadId::DogUltimateCatch));
        assert!(payload_is_ultimate_scratch(
            AttackPayloadId::FoxUltimateScratchHeavy
        ));
        assert!(payload_is_ultimate_bomb(AttackPayloadId::PandaUltimateBomb));
        assert!(payload_is_jump_fish(AttackPayloadId::DogJumpFishShot));
        assert!(payload_is_jump_spike(AttackPayloadId::PandaJumpDrop));
        assert!(payload_is_ultimate_catch(AttackPayloadId::PigUltimateCatch));
        assert!(payload_is_ultimate_scratch(
            AttackPayloadId::PigUltimateScratchHeavy
        ));
        assert!(payload_is_ultimate_bomb(AttackPayloadId::PigUltimateBomb));
        assert!(payload_is_jump_fish(AttackPayloadId::PigHamLob));
        assert!(payload_is_jump_spike(AttackPayloadId::PigJumpBellyDrop));
        assert!(payload_is_ultimate_catch(AttackPayloadId::BeeUltimateCatch));
        assert!(payload_is_ultimate_scratch(
            AttackPayloadId::BeeUltimateScratchHeavy
        ));
        assert!(payload_is_ultimate_bomb(AttackPayloadId::BeeUltimateBomb));
        let bee_air = attack_payload_definition(AttackPayloadId::BeeAirSting);
        assert_eq!(bee_air.shape_id, AttackShapeId::CompactThrust);
        assert_eq!(
            bee_air.reaction_family,
            ReactionFamilyId::ShortStandingStagger
        );
        assert_eq!(bee_air.vertical_knockback, 0.0);
        assert!(payload_is_jump_spike(AttackPayloadId::BeeHiveDive));
        assert!(payload_is_ultimate_catch(
            AttackPayloadId::PenguinUltimateCatch
        ));
        assert!(payload_is_ultimate_scratch(
            AttackPayloadId::PenguinUltimateScratchHeavy
        ));
        assert!(payload_is_ultimate_bomb(
            AttackPayloadId::PenguinUltimateBomb
        ));
        assert!(payload_is_ultimate_bomb(
            AttackPayloadId::PenguinUltimateSlopeCrash
        ));
        assert!(payload_is_jump_spike(
            AttackPayloadId::PenguinFrozenFishDive
        ));
        let penguin_peck = attack_payload_definition(AttackPayloadId::PenguinPopsiclePeck);
        assert_eq!(penguin_peck.shape_id, AttackShapeId::CompactThrust);
        assert_eq!(
            penguin_peck.reaction_family,
            ReactionFamilyId::ShortStandingStagger
        );
        assert_eq!(penguin_peck.vertical_knockback, 0.0);
    }

    #[test]
    fn character_orders_include_authored_followups_for_buffered_chains() {
        let catalog = CharacterMoveCatalog::default();
        let cases: &[(CharacterKind, &[TechniqueId])] = &[
            (
                CharacterKind::Cat,
                &[
                    TechniqueId::CatLight2,
                    TechniqueId::CatComboFinisher,
                    TechniqueId::CatHeavy2,
                ],
            ),
            (
                CharacterKind::Pig,
                &[TechniqueId::PigLight2, TechniqueId::PigComboFinisher],
            ),
            (
                CharacterKind::Dog,
                &[
                    TechniqueId::DogLight2,
                    TechniqueId::DogComboFinisher,
                    TechniqueId::DogHeavy2,
                ],
            ),
            (
                CharacterKind::Fox,
                &[
                    TechniqueId::FoxLight2,
                    TechniqueId::FoxComboFinisher,
                    TechniqueId::FoxHeavy2,
                ],
            ),
            (
                CharacterKind::Panda,
                &[
                    TechniqueId::PandaLight2,
                    TechniqueId::PandaComboFinisher,
                    TechniqueId::PandaHeavy2,
                ],
            ),
            (
                CharacterKind::Penguin,
                &[
                    TechniqueId::PenguinLight2,
                    TechniqueId::PenguinHeavy2,
                    TechniqueId::PenguinDashHeavy,
                ],
            ),
            (
                CharacterKind::Chick,
                &[
                    TechniqueId::ChickLight2,
                    TechniqueId::ChickComboFinisher,
                    TechniqueId::ChickHeavy2,
                ],
            ),
        ];

        for (character, required) in cases {
            let ordered = catalog.ordered_techniques(*character);
            for technique in *required {
                assert!(
                    ordered.contains(technique),
                    "{character:?} is missing authored followup {technique:?}"
                );
            }
        }
    }

    #[test]
    fn non_cat_roster_routes_do_not_include_cat_authored_attacks() {
        let catalog = CharacterMoveCatalog::default();
        let cat_attacks = [
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
        ];

        for character in [
            CharacterKind::Pig,
            CharacterKind::Dog,
            CharacterKind::Fox,
            CharacterKind::Panda,
            CharacterKind::Bee,
            CharacterKind::Penguin,
            CharacterKind::Chick,
        ] {
            for cat_attack in cat_attacks {
                assert!(
                    !catalog.ordered_techniques(character).contains(&cat_attack),
                    "{character:?} unexpectedly includes {cat_attack:?}"
                );
            }
        }
    }

    #[test]
    fn every_character_owns_core_authored_attack_routes() {
        let catalog = CharacterMoveCatalog::default();
        let cases: &[(CharacterKind, &[TechniqueId])] = &[
            (
                CharacterKind::Cat,
                &[
                    TechniqueId::CatLight1,
                    TechniqueId::CatLight2,
                    TechniqueId::CatComboFinisher,
                    TechniqueId::CatHeavy,
                    TechniqueId::CatHeavy2,
                    TechniqueId::CatDashAttack,
                    TechniqueId::CatJumpAttack,
                    TechniqueId::CatJumpHeavy,
                    TechniqueId::CatUltimateStartup,
                    TechniqueId::CatUltimateRush,
                ],
            ),
            (
                CharacterKind::Pig,
                &[
                    TechniqueId::PigLight1,
                    TechniqueId::PigLight2,
                    TechniqueId::PigComboFinisher,
                    TechniqueId::PigHeavy,
                    TechniqueId::PigDashAttack,
                    TechniqueId::PigJumpAttack,
                    TechniqueId::PigJumpHeavy,
                    TechniqueId::PigUltimateStartup,
                    TechniqueId::PigUltimateRush,
                ],
            ),
            (
                CharacterKind::Dog,
                &[
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
                ],
            ),
            (
                CharacterKind::Fox,
                &[
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
                ],
            ),
            (
                CharacterKind::Panda,
                &[
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
                ],
            ),
            (
                CharacterKind::Bee,
                &[
                    TechniqueId::BeeLight1,
                    TechniqueId::BeeHeavy2,
                    TechniqueId::BeeDashAttack,
                    TechniqueId::BeeJumpAttack,
                    TechniqueId::BeeJumpHeavy,
                    TechniqueId::BeeUltimateStartup,
                ],
            ),
            (
                CharacterKind::Penguin,
                &[
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
                ],
            ),
            (
                CharacterKind::Chick,
                &[
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
                ],
            ),
        ];

        for (character, expected_routes) in cases {
            for route in *expected_routes {
                assert!(
                    catalog.allows_technique(*character, *route),
                    "{character:?} is missing {route:?}"
                );
            }
        }
    }

    #[test]
    fn generic_action_lookup_does_not_resolve_authored_attacks() {
        for action in [
            FighterAction::LightAttack1,
            FighterAction::LightAttack2,
            FighterAction::ComboFinisher,
            FighterAction::HeavyAttack,
            FighterAction::HeavyAttack2,
            FighterAction::UltimateStartup,
            FighterAction::UltimateRush,
            FighterAction::DashAttack,
            FighterAction::JumpAttack,
            FighterAction::JumpHeavyAttack,
        ] {
            assert!(
                technique_definition(action).is_none(),
                "{action:?} should resolve through a concrete TechniqueId"
            );
        }

        assert!(technique_definition(FighterAction::GuardStep).is_some());
        assert!(technique_definition(FighterAction::LandingRecovery).is_some());
    }

    #[test]
    fn pig_heavy_is_charged_half_circle_swing() {
        let catalog = CharacterMoveCatalog::default();
        let pig = LoadoutContext::for_character(
            CharacterKind::Pig,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );

        assert_eq!(
            raw_technique_for_loadout_in_catalog(TechniqueButton::B, true, pig, &catalog)
                .unwrap()
                .id,
            TechniqueId::PigHeavy
        );
        assert_eq!(
            technique_slot_for_loadout(CharacterMoveSlot::DashHeavy, pig, &catalog)
                .unwrap()
                .id,
            TechniqueId::PigHeavy
        );
        assert!(!catalog.allows_technique(CharacterKind::Pig, TechniqueId::PigHeavy2));
        assert!(
            chained_technique_for_context_in_catalog(
                TechniqueMatchContext {
                    previous: Some(TechniqueId::PigHeavy),
                    button: TechniqueButton::B,
                    elapsed: 0.16,
                    style: FighterStyleKind::Anchor,
                    loadout: pig,
                    grounded: true,
                    confirmed_hit: false,
                    cancel_window_open: true,
                    branch_window_open: true,
                    current_action: FighterAction::HeavyAttack,
                },
                &catalog,
            )
            .is_none()
        );

        let heavy =
            technique_definition_for_loadout_id_in_catalog(TechniqueId::PigHeavy, pig, &catalog)
                .unwrap();
        let charged = heavy
            .script
            .events
            .iter()
            .find_map(|event| match event.kind {
                MoveTimelineEventKind::ChargedAttack { tap, partial, full } => {
                    Some((event.at_ms, tap, partial, full))
                }
                _ => None,
            })
            .unwrap();

        assert_eq!(
            charged,
            (
                360,
                AttackPayloadId::PigHamSwingTap,
                AttackPayloadId::PigHamSwingPartial,
                AttackPayloadId::PigHamSwingFull
            )
        );
        assert_eq!(
            charged_payload_for_elapsed(
                0.1,
                AttackPayloadId::PigHamSwingTap,
                AttackPayloadId::PigHamSwingPartial,
                AttackPayloadId::PigHamSwingFull,
            ),
            AttackPayloadId::PigHamSwingTap
        );
        assert_eq!(
            charged_payload_for_elapsed(
                0.5,
                AttackPayloadId::PigHamSwingTap,
                AttackPayloadId::PigHamSwingPartial,
                AttackPayloadId::PigHamSwingFull,
            ),
            AttackPayloadId::PigHamSwingPartial
        );
        assert_eq!(
            charged_payload_for_elapsed(
                0.95,
                AttackPayloadId::PigHamSwingTap,
                AttackPayloadId::PigHamSwingPartial,
                AttackPayloadId::PigHamSwingFull,
            ),
            AttackPayloadId::PigHamSwingFull
        );

        let tap = attack_payload_definition(AttackPayloadId::PigHamSwingTap);
        let partial = attack_payload_definition(AttackPayloadId::PigHamSwingPartial);
        let full = attack_payload_definition(AttackPayloadId::PigHamSwingFull);
        let shape = attack_shape_definition(full.shape_id);
        assert!(full.damage > tap.damage);
        assert!(tap.knockback < partial.knockback);
        assert!(partial.knockback < full.knockback);
        assert_eq!(tap.reaction_family, ReactionFamilyId::HeavyStandingStagger);
        assert_eq!(partial.reaction_family, ReactionFamilyId::SlidingKnockdown);
        assert_eq!(full.reaction_family, ReactionFamilyId::GroundBounceDown);
        assert_eq!(partial.damage_profile, DamageProfileId::GroundBounce);
        assert_eq!(full.damage_profile, DamageProfileId::GroundBounce);
        assert!(full.knockback >= HEAVY_KNOCKBACK * 1.6);
        assert!(full.vertical_knockback > partial.vertical_knockback);
        assert_eq!(full.shape_id, AttackShapeId::PigHalfCircleSwing);
        assert!(shape.curved);
        assert!(shape.parented);
        assert!(shape.path.len() >= 6);
    }

    #[test]
    fn active_authored_attack_requires_concrete_technique_identity() {
        let catalog = CharacterMoveCatalog::default();
        let pig = LoadoutContext::for_character(
            CharacterKind::Pig,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );

        assert!(
            active_technique_definition_in_catalog(
                FighterAction::HeavyAttack2,
                None,
                pig,
                &catalog
            )
            .is_none()
        );
        assert!(
            active_technique_definition_in_catalog(
                FighterAction::HeavyAttack2,
                Some(TechniqueId::CatHeavy2),
                pig,
                &catalog
            )
            .is_none()
        );

        assert!(
            active_technique_definition_in_catalog(
                FighterAction::HeavyAttack2,
                Some(TechniqueId::PigHeavy2),
                pig,
                &catalog,
            )
            .is_none()
        );
    }

    #[test]
    fn pig_air_and_light_routes_reuse_cat_slice_and_meat_launcher() {
        let catalog = CharacterMoveCatalog::default();
        let pig = LoadoutContext::for_character(
            CharacterKind::Pig,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );

        let light1 =
            technique_definition_for_loadout_id_in_catalog(TechniqueId::PigLight1, pig, &catalog)
                .unwrap();
        let light2 =
            technique_definition_for_loadout_id_in_catalog(TechniqueId::PigLight2, pig, &catalog)
                .unwrap();
        let light_attacks: Vec<(u32, AttackPayloadId)> = light1
            .script
            .events
            .iter()
            .chain(light2.script.events.iter())
            .filter_map(|event| match event.kind {
                MoveTimelineEventKind::Attack(payload) => Some((event.at_ms, payload)),
                _ => None,
            })
            .collect();
        assert_eq!(
            light_attacks,
            vec![
                (440, AttackPayloadId::AsBeat1),
                (440, AttackPayloadId::AssBeat1)
            ]
        );

        let jump_light =
            technique_slot_for_loadout(CharacterMoveSlot::JumpLight, pig, &catalog).unwrap();
        assert!(jump_light.script.events.iter().any(|event| matches!(
            event.kind,
            MoveTimelineEventKind::Attack(AttackPayloadId::JumpSpike)
        )));

        let jump_heavy =
            technique_slot_for_loadout(CharacterMoveSlot::JumpHeavy, pig, &catalog).unwrap();
        assert!(jump_heavy.script.events.iter().any(|event| matches!(
            event.kind,
            MoveTimelineEventKind::Attack(AttackPayloadId::PigAirMeatSlam)
        )));
        assert!(jump_heavy.script.events.iter().any(|event| matches!(
            event.kind,
            MoveTimelineEventKind::Attack(AttackPayloadId::PigAirMeatSlam) if event.at_ms == 0
        )));
        assert_eq!(
            attack_payload_definition(AttackPayloadId::PigAirMeatSlam).reaction_family,
            ReactionFamilyId::LauncherDown
        );
        assert_eq!(
            attack_payload_definition(AttackPayloadId::PigAirMeatSlam).vertical_knockback,
            10.56
        );
        let ham_slam = attack_payload_definition(AttackPayloadId::PigHamSlam);
        assert_eq!(ham_slam.shape_id, AttackShapeId::PigMeatSlam);
        assert_eq!(
            ham_slam.reaction_family,
            ReactionFamilyId::GroundedDownGetup
        );
        assert_eq!(ham_slam.knockback, 0.0);
        assert_eq!(ham_slam.vertical_knockback, 0.0);
    }

    #[test]
    fn bee_routes_use_ranged_worker_and_homing_shots() {
        let catalog = CharacterMoveCatalog::default();
        let bee = LoadoutContext::for_character(
            CharacterKind::Bee,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );

        let light1 =
            technique_definition_for_loadout_id_in_catalog(TechniqueId::BeeLight1, bee, &catalog)
                .unwrap();
        let heavy2 =
            technique_definition_for_loadout_id_in_catalog(TechniqueId::BeeHeavy2, bee, &catalog)
                .unwrap();
        let jump_heavy = technique_definition_for_loadout_id_in_catalog(
            TechniqueId::BeeJumpHeavy,
            bee,
            &catalog,
        )
        .unwrap();
        let ultimate =
            technique_slot_for_loadout(CharacterMoveSlot::UltimateStartup, bee, &catalog).unwrap();
        let dash_light =
            technique_slot_for_loadout(CharacterMoveSlot::DashLight, bee, &catalog).unwrap();
        let dash_heavy =
            technique_slot_for_loadout(CharacterMoveSlot::DashHeavy, bee, &catalog).unwrap();

        assert_eq!(
            raw_technique_for_loadout_in_catalog(TechniqueButton::A, true, bee, &catalog)
                .unwrap()
                .id,
            TechniqueId::BeeLight1
        );
        assert_eq!(
            raw_technique_for_loadout_in_catalog(TechniqueButton::B, true, bee, &catalog)
                .unwrap()
                .id,
            TechniqueId::BeeHeavy2
        );
        assert_eq!(light1.status, TechniqueStatus::Grounded);
        assert!(light1.chain_rule.is_none());
        assert!(heavy2.chain_rule.is_none());
        assert_eq!(dash_light.id, TechniqueId::BeeLight1);
        assert_eq!(dash_heavy.id, TechniqueId::BeeHeavy2);
        assert!(
            technique_definition_for_loadout_id_in_catalog(TechniqueId::BeeLight2, bee, &catalog)
                .is_none()
        );
        assert!(
            technique_definition_for_loadout_id_in_catalog(
                TechniqueId::BeeComboFinisher,
                bee,
                &catalog
            )
            .is_none()
        );
        assert!(
            technique_definition_for_loadout_id_in_catalog(TechniqueId::BeeHeavy, bee, &catalog)
                .is_none()
        );
        assert!(
            chained_technique_for_context_in_catalog(
                TechniqueMatchContext {
                    previous: Some(TechniqueId::BeeLight1),
                    button: TechniqueButton::A,
                    elapsed: 0.2,
                    style: FighterStyleKind::Anchor,
                    loadout: bee,
                    grounded: true,
                    confirmed_hit: true,
                    cancel_window_open: true,
                    branch_window_open: true,
                    current_action: FighterAction::LightAttack1,
                },
                &catalog,
            )
            .is_none()
        );
        assert_eq!(ultimate.status, TechniqueStatus::Grounded);
        assert_eq!(ultimate.stamina_cost, ULTIMATE_STAMINA_COST);
        assert!(light1.script.events.iter().any(|event| matches!(
            event.kind,
            MoveTimelineEventKind::SpawnBeeSkill(BeeSkillId::WorkerSwarm)
        )));
        assert!(!light1.script.events.iter().any(|event| matches!(
            event.kind,
            MoveTimelineEventKind::Attack(AttackPayloadId::BeeNeedleTap)
        )));
        assert!(heavy2.script.events.iter().any(|event| matches!(
            event.kind,
            MoveTimelineEventKind::SpawnBeeSkill(BeeSkillId::HomingSting)
        )));
        assert!(!heavy2.script.events.iter().any(|event| matches!(
            event.kind,
            MoveTimelineEventKind::Attack(AttackPayloadId::BeeHiveLauncher)
        )));
        let hive_launcher = attack_payload_definition(AttackPayloadId::BeeHiveLauncher);
        let worker_sting = attack_payload_definition(AttackPayloadId::BeeWorkerSting);
        let homing_sting = attack_payload_definition(AttackPayloadId::BeeHomingSting);

        assert_eq!(hive_launcher.shape_id, AttackShapeId::ProjectileBolt);
        assert_ne!(
            hive_launcher.reaction_family,
            ReactionFamilyId::LauncherDown
        );
        assert_ne!(
            hive_launcher.reaction_family,
            ReactionFamilyId::GroundBounceDown
        );
        assert_eq!(hive_launcher.vertical_knockback, 0.0);
        assert_ne!(worker_sting.reaction_family, ReactionFamilyId::LauncherDown);
        assert_eq!(worker_sting.vertical_knockback, 0.0);
        assert_ne!(homing_sting.reaction_family, ReactionFamilyId::LauncherDown);
        assert_eq!(homing_sting.vertical_knockback, 0.0);
        assert_eq!(
            attack_payload_definition(AttackPayloadId::BeeHoneyGlob).shape_id,
            AttackShapeId::ProjectileBolt
        );
        assert_eq!(
            attack_payload_definition(AttackPayloadId::BeeHoneyPuddle).shape_id,
            AttackShapeId::HazardField
        );
        assert_eq!(heavy2.action, FighterAction::HeavyAttack2);
        assert!(jump_heavy.script.events.iter().any(|event| matches!(
            event.kind,
            MoveTimelineEventKind::SpawnBeeSkill(BeeSkillId::HoneyGlob)
        )));
        assert!(!jump_heavy.script.events.iter().any(|event| matches!(
            event.kind,
            MoveTimelineEventKind::Attack(AttackPayloadId::BeeHiveDive)
        )));
    }

    #[test]
    fn penguin_routes_spawn_arena_shaper_skills() {
        let catalog = CharacterMoveCatalog::default();
        let penguin = LoadoutContext::for_character(
            CharacterKind::Penguin,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );

        let combo = technique_definition_for_loadout_id_in_catalog(
            TechniqueId::PenguinComboFinisher,
            penguin,
            &catalog,
        )
        .unwrap();
        let heavy = technique_definition_for_loadout_id_in_catalog(
            TechniqueId::PenguinHeavy,
            penguin,
            &catalog,
        )
        .unwrap();
        let light1 = technique_definition_for_loadout_id_in_catalog(
            TechniqueId::PenguinLight1,
            penguin,
            &catalog,
        )
        .unwrap();
        let light2 = technique_definition_for_loadout_id_in_catalog(
            TechniqueId::PenguinLight2,
            penguin,
            &catalog,
        )
        .unwrap();
        let dash_light =
            technique_slot_for_loadout(CharacterMoveSlot::DashLight, penguin, &catalog).unwrap();
        let dash_heavy =
            technique_slot_for_loadout(CharacterMoveSlot::DashHeavy, penguin, &catalog).unwrap();
        let heavy2 = technique_definition_for_loadout_id_in_catalog(
            TechniqueId::PenguinHeavy2,
            penguin,
            &catalog,
        )
        .unwrap();
        let jump_light =
            technique_slot_for_loadout(CharacterMoveSlot::JumpLight, penguin, &catalog).unwrap();
        let jump_heavy =
            technique_slot_for_loadout(CharacterMoveSlot::JumpHeavy, penguin, &catalog).unwrap();
        let ultimate_startup =
            technique_slot_for_loadout(CharacterMoveSlot::UltimateStartup, penguin, &catalog)
                .unwrap();
        let ultimate_rush =
            technique_slot_for_loadout(CharacterMoveSlot::UltimateRush, penguin, &catalog).unwrap();

        assert!(combo.script.events.iter().any(|event| matches!(
            event.kind,
            MoveTimelineEventKind::SpawnPenguinSkill(PenguinSkillId::IceTrail)
        )));
        let light1_skill_events: Vec<_> = light1
            .script
            .events
            .iter()
            .filter_map(|event| match event.kind {
                MoveTimelineEventKind::SpawnPenguinSkill(skill) => Some((event.at_ms, skill)),
                _ => None,
            })
            .collect();
        let light2_skill_events: Vec<_> = light2
            .script
            .events
            .iter()
            .filter_map(|event| match event.kind {
                MoveTimelineEventKind::SpawnPenguinSkill(skill) => Some((event.at_ms, skill)),
                _ => None,
            })
            .collect();
        let light_attack_events: Vec<_> = light1
            .script
            .events
            .iter()
            .chain(light2.script.events.iter())
            .filter_map(|event| match event.kind {
                MoveTimelineEventKind::Attack(payload) => Some(payload),
                _ => None,
            })
            .collect();
        assert_eq!(
            light1_skill_events,
            vec![(115, PenguinSkillId::SnowflakeShot)]
        );
        assert_eq!(
            light2_skill_events,
            vec![(130, PenguinSkillId::SnowflakeShot)]
        );
        assert_eq!(light1.script.recover_ms, 160);
        assert_eq!(light2.script.recover_ms, 175);
        assert_eq!(light1.branch_window, Some(MsTimingWindow::closed(140, 150)));
        assert_eq!(light2.branch_window, Some(MsTimingWindow::closed(155, 165)));
        assert_eq!(
            light2.chain_rule.as_ref().map(|rule| rule.window),
            Some(MsTimingWindow::closed(140, 150))
        );
        assert!(combo.chain_rule.is_none());
        assert!(
            chained_technique_for_context_in_catalog(
                TechniqueMatchContext {
                    previous: Some(TechniqueId::PenguinLight2),
                    button: TechniqueButton::A,
                    elapsed: 0.16,
                    style: FighterStyleKind::Anchor,
                    loadout: penguin,
                    grounded: true,
                    confirmed_hit: true,
                    cancel_window_open: true,
                    branch_window_open: true,
                    current_action: FighterAction::LightAttack2,
                },
                &catalog,
            )
            .is_none()
        );
        assert!(light_attack_events.is_empty());
        let heavy_skill_events: Vec<_> = heavy
            .script
            .events
            .iter()
            .filter_map(|event| match event.kind {
                MoveTimelineEventKind::SpawnPenguinSkill(skill) => Some((event.at_ms, skill)),
                _ => None,
            })
            .collect();
        let heavy_attacks: Vec<_> = heavy
            .script
            .events
            .iter()
            .filter_map(|event| match event.kind {
                MoveTimelineEventKind::Attack(payload) => Some(payload),
                _ => None,
            })
            .collect();
        assert_eq!(heavy_skill_events, vec![(90, PenguinSkillId::SnowmanDrop)]);
        assert!(heavy_attacks.is_empty());
        assert!(
            chained_technique_for_context_in_catalog(
                TechniqueMatchContext {
                    previous: Some(TechniqueId::PenguinHeavy),
                    button: TechniqueButton::B,
                    elapsed: 0.24,
                    style: FighterStyleKind::Anchor,
                    loadout: penguin,
                    grounded: true,
                    confirmed_hit: false,
                    cancel_window_open: true,
                    branch_window_open: true,
                    current_action: FighterAction::HeavyAttack,
                },
                &catalog,
            )
            .is_none()
        );
        let dash_light_events: Vec<_> = dash_light
            .script
            .events
            .iter()
            .map(|event| (event.at_ms, event.kind))
            .collect();
        let jump_light_events: Vec<_> = jump_light
            .script
            .events
            .iter()
            .map(|event| (event.at_ms, event.kind))
            .collect();
        assert_eq!(dash_light_events, jump_light_events);
        assert_eq!(dash_light.script.recover_ms, jump_light.script.recover_ms);
        let dash_light_skill_events: Vec<_> = dash_light
            .script
            .events
            .iter()
            .filter_map(|event| match event.kind {
                MoveTimelineEventKind::SpawnPenguinSkill(skill) => Some((event.at_ms, skill)),
                _ => None,
            })
            .collect();
        assert_eq!(
            dash_light_skill_events,
            vec![(0, PenguinSkillId::SnowflakeShot)]
        );
        let dash_light_attacks: Vec<_> = dash_light
            .script
            .events
            .iter()
            .filter_map(|event| match event.kind {
                MoveTimelineEventKind::Attack(payload) => Some((event.at_ms, payload)),
                _ => None,
            })
            .collect();
        assert!(dash_light_attacks.is_empty());
        assert_eq!(dash_heavy.id, TechniqueId::PenguinDashHeavy);
        assert_eq!(dash_heavy.action, FighterAction::DashAttack);
        let dash_heavy_skill_events: Vec<_> = dash_heavy
            .script
            .events
            .iter()
            .filter_map(|event| match event.kind {
                MoveTimelineEventKind::SpawnPenguinSkill(skill) => Some(skill),
                _ => None,
            })
            .collect();
        assert_eq!(dash_heavy_skill_events, vec![PenguinSkillId::SnowflakeShot]);
        let dash_heavy_attacks: Vec<_> = dash_heavy
            .script
            .events
            .iter()
            .filter_map(|event| match event.kind {
                MoveTimelineEventKind::Attack(payload) => Some((event.at_ms, payload)),
                _ => None,
            })
            .collect();
        assert!(dash_heavy_attacks.is_empty());
        assert!(
            !dash_heavy
                .script
                .events
                .iter()
                .any(|event| matches!(event.kind, MoveTimelineEventKind::Feedback(_, _)))
        );
        assert!(
            !dash_heavy
                .script
                .events
                .iter()
                .any(|event| matches!(event.kind, MoveTimelineEventKind::Stop))
        );
        assert!(
            !dash_heavy
                .script
                .events
                .iter()
                .any(|event| matches!(event.kind, MoveTimelineEventKind::NextTech))
        );
        let dash_heavy_motion: Vec<_> = dash_heavy
            .script
            .events
            .iter()
            .filter_map(|event| match event.kind {
                MoveTimelineEventKind::Motion { forward, lift } => Some((forward, lift)),
                _ => None,
            })
            .collect();
        let total_forward: f32 = dash_heavy_motion.iter().map(|(forward, _)| *forward).sum();
        assert!(dash_heavy_motion.is_empty());
        assert_eq!(total_forward, 0.0);
        assert_eq!(dash_heavy.script.recover_ms, 180);
        assert!(heavy2.script.events.iter().any(|event| matches!(
            event.kind,
            MoveTimelineEventKind::SpawnPenguinSkill(PenguinSkillId::SnowfortCannon)
        )));
        let jump_light_skill_events: Vec<_> = jump_light
            .script
            .events
            .iter()
            .filter_map(|event| match event.kind {
                MoveTimelineEventKind::SpawnPenguinSkill(skill) => Some((event.at_ms, skill)),
                _ => None,
            })
            .collect();
        let jump_light_attacks: Vec<_> = jump_light
            .script
            .events
            .iter()
            .filter_map(|event| match event.kind {
                MoveTimelineEventKind::Attack(payload) => Some(payload),
                _ => None,
            })
            .collect();
        assert_eq!(
            jump_light_skill_events,
            vec![(0, PenguinSkillId::SnowflakeShot)]
        );
        assert_eq!(jump_light.script.recover_ms, 160);
        assert!(jump_light_attacks.is_empty());
        let jump_heavy_skill_events: Vec<_> = jump_heavy
            .script
            .events
            .iter()
            .filter_map(|event| match event.kind {
                MoveTimelineEventKind::SpawnPenguinSkill(skill) => Some((event.at_ms, skill)),
                _ => None,
            })
            .collect();
        let jump_heavy_attacks: Vec<_> = jump_heavy
            .script
            .events
            .iter()
            .filter_map(|event| match event.kind {
                MoveTimelineEventKind::Attack(payload) => Some(payload),
                _ => None,
            })
            .collect();
        assert_eq!(
            jump_heavy_skill_events,
            vec![(0, PenguinSkillId::SnowflakeSwapShot)]
        );
        assert_eq!(jump_heavy.script.recover_ms, 140);
        assert!(jump_heavy_attacks.is_empty());
        let ultimate_startup_skill_events: Vec<_> = ultimate_startup
            .script
            .events
            .iter()
            .filter_map(|event| match event.kind {
                MoveTimelineEventKind::SpawnPenguinSkill(skill) => Some((event.at_ms, skill)),
                _ => None,
            })
            .collect();
        let ultimate_startup_attack_events: Vec<_> = ultimate_startup
            .script
            .events
            .iter()
            .filter_map(|event| match event.kind {
                MoveTimelineEventKind::Attack(payload) => Some((event.at_ms, payload)),
                _ => None,
            })
            .collect();
        let ultimate_startup_motion_events: Vec<_> = ultimate_startup
            .script
            .events
            .iter()
            .filter_map(|event| match event.kind {
                MoveTimelineEventKind::Motion { forward, lift } => {
                    Some((event.at_ms, forward, lift))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            ultimate_startup_skill_events,
            vec![(0, PenguinSkillId::UltimateIceField)]
        );
        assert!(ultimate_startup_attack_events.is_empty());
        assert!(ultimate_startup_motion_events.is_empty());

        let ultimate_rush_skill_events: Vec<_> = ultimate_rush
            .script
            .events
            .iter()
            .filter_map(|event| match event.kind {
                MoveTimelineEventKind::SpawnPenguinSkill(skill) => Some((event.at_ms, skill)),
                _ => None,
            })
            .collect();
        let ultimate_rush_attack_events: Vec<_> = ultimate_rush
            .script
            .events
            .iter()
            .filter_map(|event| match event.kind {
                MoveTimelineEventKind::Attack(payload) => Some((event.at_ms, payload)),
                _ => None,
            })
            .collect();
        let ultimate_rush_motion_events: Vec<_> = ultimate_rush
            .script
            .events
            .iter()
            .filter_map(|event| match event.kind {
                MoveTimelineEventKind::Motion { forward, lift } => {
                    Some((event.at_ms, forward, lift))
                }
                _ => None,
            })
            .collect();

        assert_eq!(
            ultimate_rush_skill_events,
            vec![(0, PenguinSkillId::SnowSlopeRide)]
        );
        assert_eq!(
            ultimate_rush_attack_events,
            vec![(0, AttackPayloadId::PenguinUltimateSlopeCrash)]
        );
        assert_eq!(
            ultimate_rush_motion_events,
            vec![(0, 7.4, 0.0), (130, 3.4, 0.0)]
        );
        assert!(!ultimate_rush.script.events.iter().any(|event| matches!(
            event.kind,
            MoveTimelineEventKind::Attack(
                AttackPayloadId::PenguinUltimateCatch
                    | AttackPayloadId::PenguinUltimateScratchLight
                    | AttackPayloadId::PenguinUltimateScratchHeavy
                    | AttackPayloadId::PenguinUltimateBomb
            ) | MoveTimelineEventKind::SpawnPenguinSkill(
                PenguinSkillId::GlacierParade | PenguinSkillId::SnowflakeBurst
            )
        )));
        let ultimate_slope = attack_payload_definition(AttackPayloadId::PenguinUltimateSlopeCrash);
        let ultimate_slope_shape = attack_shape_definition(ultimate_slope.shape_id);
        let ultimate_slope_front = ultimate_slope_shape.range
            + ultimate_slope_shape.radius
            + ultimate_slope_shape
                .path
                .iter()
                .map(|point| point[2])
                .fold(f32::MIN, f32::max);
        assert_eq!(ultimate_slope.kind, AttackKind::Ultimate);
        assert_eq!(
            ultimate_slope.shape_id,
            AttackShapeId::PenguinUltimateSlopeBody
        );
        assert_eq!(
            ultimate_slope.reaction_family,
            ReactionFamilyId::AirFishKnockdown
        );
        assert_eq!(ultimate_slope.damage_profile, DamageProfileId::UltimateRush);
        assert!(ultimate_slope.time_ms > 0);
        assert!(ultimate_slope.damage > ULTIMATE_BOMB_DAMAGE);
        assert!(ultimate_slope.knockback > 0.0);
        assert!(
            ultimate_slope.knockback
                >= (PENGUIN_SLOPE_LAUNCH_FORWARD + PENGUIN_SLOPE_EXIT_FORWARD) * 1.65
        );
        assert!(ultimate_slope.vertical_knockback > 0.0);
        assert!(ultimate_slope_shape.parented);
        assert!(ultimate_slope_shape.radius > 0.2);
        assert!(
            ultimate_slope_shape.vertical_offset_scale > 0.0
                && ultimate_slope_shape.vertical_offset_scale < 1.0
        );
        assert!(ultimate_slope_front > 0.0);
        assert!(ultimate_slope_front < 100.0);
        assert_eq!(
            attack_payload_definition(AttackPayloadId::PenguinFishTorpedo).shape_id,
            AttackShapeId::ProjectileBolt
        );
        assert_eq!(
            attack_payload_definition(AttackPayloadId::PenguinPopsicleBounce).shape_id,
            AttackShapeId::ProjectileBolt
        );
        assert_eq!(
            attack_payload_definition(AttackPayloadId::PenguinSledWake).shape_id,
            AttackShapeId::HazardField
        );
        let snowflake = attack_payload_definition(AttackPayloadId::PenguinSnowflakeShard);
        assert_eq!(snowflake.shape_id, AttackShapeId::ProjectileBolt);
        assert_eq!(snowflake.reaction_family, ReactionFamilyId::FrozenStun);
        assert_eq!(snowflake.damage, 2.0);
        assert_eq!(snowflake.knockback, 0.0);
        assert_eq!(snowflake.vertical_knockback, 0.0);
        assert!(snowflake.guardable);
        assert_eq!(
            attack_payload_definition(AttackPayloadId::PenguinSnowBoulder).shape_id,
            AttackShapeId::ProjectileBolt
        );
        let snowman = attack_payload_definition(AttackPayloadId::PenguinSnowmanDrop);
        assert_eq!(snowman.shape_id, AttackShapeId::ProjectileBolt);
        assert_eq!(snowman.reaction_family, ReactionFamilyId::FrozenStun);
        assert_eq!(snowman.damage, 2.0);
        assert_eq!(snowman.knockback, 0.0);
        assert_eq!(snowman.vertical_knockback, 0.0);
        assert!(snowman.guardable);
        assert_eq!(
            attack_payload_definition(AttackPayloadId::PenguinBodySlamShockwave).shape_id,
            AttackShapeId::ShockwaveRing
        );
    }

    #[test]
    fn chick_routes_use_guardable_egg_projectile_kit() {
        let catalog = CharacterMoveCatalog::default();
        let chick = LoadoutContext::for_character(
            CharacterKind::Chick,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );

        assert_eq!(TechniqueId::ChickLight1.owner(), Some(CharacterKind::Chick));
        assert_eq!(TechniqueId::ChickUltimateStartup.label(), "chick_egg_burst");
        assert_eq!(TechniqueId::ChickLight1.label(), "chick_orbit_egg_launch");
        assert_eq!(TechniqueId::ChickHeavy.label(), "chick_orbit_egg");
        assert_eq!(
            TechniqueId::ChickDashAttack.label(),
            "chick_dash_backstep_c"
        );
        assert_eq!(TechniqueId::ChickDashHeavy.label(), "chick_dash_backstep_x");
        assert_eq!(TechniqueId::ChickJumpAttack.label(), "chick_updraft_glide");
        assert_eq!(TechniqueId::ChickJumpHeavy.label(), "chick_fresh_egg_ride");

        let light1 = technique_definition_for_loadout_id_in_catalog(
            TechniqueId::ChickLight1,
            chick,
            &catalog,
        )
        .unwrap();
        let light2 = technique_definition_for_loadout_id_in_catalog(
            TechniqueId::ChickLight2,
            chick,
            &catalog,
        )
        .unwrap();
        let finisher = technique_definition_for_loadout_id_in_catalog(
            TechniqueId::ChickComboFinisher,
            chick,
            &catalog,
        )
        .unwrap();
        let heavy = technique_definition_for_loadout_id_in_catalog(
            TechniqueId::ChickHeavy,
            chick,
            &catalog,
        )
        .unwrap();
        let heavy2 = technique_definition_for_loadout_id_in_catalog(
            TechniqueId::ChickHeavy2,
            chick,
            &catalog,
        )
        .unwrap();
        let dash_light =
            technique_slot_for_loadout(CharacterMoveSlot::DashLight, chick, &catalog).unwrap();
        let dash_heavy =
            technique_slot_for_loadout(CharacterMoveSlot::DashHeavy, chick, &catalog).unwrap();
        let jump_light =
            technique_slot_for_loadout(CharacterMoveSlot::JumpLight, chick, &catalog).unwrap();
        let jump_heavy =
            technique_slot_for_loadout(CharacterMoveSlot::JumpHeavy, chick, &catalog).unwrap();
        let ultimate =
            technique_slot_for_loadout(CharacterMoveSlot::UltimateStartup, chick, &catalog)
                .unwrap();

        assert_eq!(light1.action, FighterAction::LightAttack1);
        assert_eq!(
            light2.chain_rule.unwrap().window,
            MsTimingWindow::closed(90, 250)
        );
        assert_eq!(
            finisher.chain_rule.unwrap().window,
            MsTimingWindow::closed(130, 350)
        );
        assert_eq!(
            heavy2.chain_rule.unwrap().window,
            MsTimingWindow::closed(120, 520)
        );
        assert_eq!(heavy.action, FighterAction::HeavyAttack);
        assert_eq!(heavy.stamina_cost, CHICK_X_STAMINA_COST);
        assert_eq!(heavy.stamina_cost, MAX_STAMINA * 0.15);
        assert!(light1.script.events.iter().any(|event| matches!(
            event.kind,
            MoveTimelineEventKind::SpawnChickSkill(ChickSkillId::OrbitEggLaunch)
        )));
        assert!(!light1.script.events.iter().any(|event| matches!(
            event.kind,
            MoveTimelineEventKind::SpawnChickSkill(ChickSkillId::ShellPeck)
                | MoveTimelineEventKind::NextTech
        )));
        assert!(light2.script.events.iter().any(|event| matches!(
            event.kind,
            MoveTimelineEventKind::SpawnChickSkill(ChickSkillId::SunnyFlip)
        )));
        assert!(finisher.script.events.iter().any(|event| matches!(
            event.kind,
            MoveTimelineEventKind::Attack(AttackPayloadId::ChickShellScramble)
        )));
        assert!(heavy.script.events.iter().any(|event| matches!(
            event.kind,
            MoveTimelineEventKind::SpawnChickSkill(ChickSkillId::OrbitEgg)
        )));
        assert!(!heavy.script.events.iter().any(|event| matches!(
            event.kind,
            MoveTimelineEventKind::SpawnChickSkill(ChickSkillId::EggCupMortar)
        )));
        assert!(heavy2.script.events.iter().any(|event| matches!(
            event.kind,
            MoveTimelineEventKind::SpawnChickSkill(ChickSkillId::EggplantRoll)
        )));
        assert_eq!(dash_light.id, TechniqueId::ChickDashAttack);
        assert_eq!(dash_light.script.id, "chick_dash_backstep_c.sc");
        assert_eq!(dash_light.script.recover_ms, CHICK_DASH_BACKSTEP_RECOVER_MS);
        assert!(dash_light.script.events.iter().any(|event| matches!(
            event.kind,
            MoveTimelineEventKind::Stop
        ) && event.at_ms
            == CHICK_DASH_BACKSTEP_STOP_MS));
        assert!(!dash_light.script.events.iter().any(|event| matches!(
            event.kind,
            MoveTimelineEventKind::Attack(_)
                | MoveTimelineEventKind::SpawnChickSkill(_)
                | MoveTimelineEventKind::Motion { .. }
        )));
        assert_eq!(dash_heavy.id, TechniqueId::ChickDashHeavy);
        assert_eq!(dash_heavy.script.id, "chick_dash_backstep_x.sc");
        assert_eq!(dash_heavy.script.recover_ms, CHICK_DASH_BACKSTEP_RECOVER_MS);
        assert!(dash_heavy.script.events.iter().any(|event| matches!(
            event.kind,
            MoveTimelineEventKind::Stop
        ) && event.at_ms
            == CHICK_DASH_BACKSTEP_STOP_MS));
        assert!(!dash_heavy.script.events.iter().any(|event| matches!(
            event.kind,
            MoveTimelineEventKind::Attack(_)
                | MoveTimelineEventKind::SpawnChickSkill(_)
                | MoveTimelineEventKind::Motion { .. }
        )));
        assert_eq!(jump_light.script.id, "chick_updraft_glide.sc");
        assert_eq!(jump_light.script.recover_ms, 620);
        assert!(
            jump_light
                .script
                .events
                .iter()
                .any(|event| matches!(event.kind, MoveTimelineEventKind::Feedback(_, _)))
        );
        assert!(
            jump_light
                .script
                .events
                .iter()
                .any(|event| matches!(event.kind, MoveTimelineEventKind::Recover)
                    && event.at_ms == 620)
        );
        assert!(!jump_light.script.events.iter().any(|event| matches!(
            event.kind,
            MoveTimelineEventKind::SpawnChickSkill(_) | MoveTimelineEventKind::Attack(_)
        )));
        assert!(jump_heavy.script.events.iter().any(|event| matches!(
            event.kind,
            MoveTimelineEventKind::SpawnChickSkill(ChickSkillId::FreshEggRide)
        )));
        assert!(!jump_heavy.script.events.iter().any(|event| matches!(
            event.kind,
            MoveTimelineEventKind::SpawnChickSkill(
                ChickSkillId::FreshEggDrop | ChickSkillId::SunnySideSplash
            ) | MoveTimelineEventKind::Attack(_)
        )));

        let ultimate_skills: Vec<_> = ultimate
            .script
            .events
            .iter()
            .filter_map(|event| match event.kind {
                MoveTimelineEventKind::SpawnChickSkill(skill) => Some((event.at_ms, skill)),
                _ => None,
            })
            .collect();
        let ultimate_attacks: Vec<_> = ultimate
            .script
            .events
            .iter()
            .filter_map(|event| match event.kind {
                MoveTimelineEventKind::Attack(payload) => Some(payload),
                _ => None,
            })
            .collect();

        assert_eq!(ultimate.action, FighterAction::UltimateStartup);
        assert_eq!(ultimate.stamina_cost, ULTIMATE_STAMINA_COST);
        assert_eq!(ultimate.script.id, "chick_egg_burst.sc");
        assert_eq!(ultimate.script.recover_ms, 560);
        assert_eq!(ultimate_skills, vec![(100, ChickSkillId::UltimateEggBurst)]);
        assert!(ultimate_attacks.is_empty());
        assert!(
            technique_slot_for_loadout(CharacterMoveSlot::UltimateRush, chick, &catalog).is_none()
        );

        let shell = attack_payload_definition(AttackPayloadId::ChickShellChip);
        let disc = attack_payload_definition(AttackPayloadId::ChickFriedEggDisc);
        let mortar = attack_payload_definition(AttackPayloadId::ChickEggCupMortar);
        let orbit = attack_payload_definition(AttackPayloadId::ChickOrbitEgg);
        let launch = attack_payload_definition(AttackPayloadId::ChickOrbitEggLaunch);
        let eggplant = attack_payload_definition(AttackPayloadId::ChickEggplantRoll);
        let splash = attack_payload_definition(AttackPayloadId::ChickSunnySplash);
        let omelet = attack_payload_definition(AttackPayloadId::ChickOmeletField);
        let scoot = attack_payload_definition(AttackPayloadId::ChickShellScoot);
        let scramble = attack_payload_definition(AttackPayloadId::ChickShellScramble);

        for payload in [
            shell, disc, mortar, orbit, launch, eggplant, splash, omelet, scoot, scramble,
        ] {
            assert!(payload.guardable, "{:?} should stay guardable", payload.id);
        }
        assert_eq!(shell.shape_id, AttackShapeId::ProjectileBolt);
        assert_eq!(disc.shape_id, AttackShapeId::ProjectileBolt);
        assert_eq!(mortar.reaction_family, ReactionFamilyId::LightAirPop);
        assert_eq!(orbit.shape_id, AttackShapeId::ProjectileBolt);
        assert_eq!(
            orbit.reaction_family,
            ReactionFamilyId::ShortStandingStagger
        );
        assert_eq!(orbit.damage, 0.75);
        assert_eq!(orbit.knockback, 0.6);
        assert_eq!(orbit.vertical_knockback, 0.0);
        assert_eq!(launch.shape_id, AttackShapeId::ProjectileBolt);
        assert_eq!(launch.reaction_family, ReactionFamilyId::SlidingKnockdown);
        assert_eq!(launch.damage_profile, DamageProfileId::DashBody);
        assert_eq!(launch.damage, 8.0);
        assert_eq!(launch.knockback, 7.2);
        assert_eq!(splash.shape_id, AttackShapeId::HazardField);
        assert_eq!(omelet.shape_id, AttackShapeId::HazardField);
        assert_eq!(scramble.reaction_family, ReactionFamilyId::GroundBounceDown);
        assert!(shell.damage < disc.damage);
        assert!(eggplant.knockback > disc.knockback);
        assert!(scramble.damage < COMBO_FINISHER_DAMAGE);
        assert!(scoot.damage < DASH_ATTACK_DAMAGE);
    }

    #[test]
    fn bee_ultimate_summons_area_swarm_without_catch_confirm() {
        let ultimate = technique_definition_by_id(TechniqueId::BeeUltimateStartup).unwrap();
        let skill_events: Vec<_> = ultimate
            .script
            .events
            .iter()
            .filter_map(|event| match event.kind {
                MoveTimelineEventKind::SpawnBeeSkill(skill) => Some((event.at_ms, skill)),
                _ => None,
            })
            .collect();
        let attack_events: Vec<_> = ultimate
            .script
            .events
            .iter()
            .filter_map(|event| match event.kind {
                MoveTimelineEventKind::Attack(payload) => Some((event.at_ms, payload)),
                _ => None,
            })
            .collect();

        assert_eq!(ultimate.action, FighterAction::UltimateStartup);
        assert_eq!(ultimate.button, TechniqueButton::Ultimate);
        assert_eq!(ultimate.status, TechniqueStatus::Grounded);
        assert_eq!(ultimate.stamina_cost, ULTIMATE_STAMINA_COST);
        assert_eq!(ultimate.stamina_cost, MAX_STAMINA * 0.5);
        assert_eq!(skill_events, vec![(120, BeeSkillId::UltimateSwarm)]);
        assert!(attack_events.is_empty());
        assert_eq!(
            attack_payload_definition(AttackPayloadId::BeeUltimateSwarmTick).damage,
            10.0
        );
        assert!(!ultimate.script.events.iter().any(|event| matches!(
            event.kind,
            MoveTimelineEventKind::Attack(AttackPayloadId::BeeUltimateCatch)
        )));
        assert!(!ultimate.script.events.iter().any(|event| matches!(
            event.kind,
            MoveTimelineEventKind::Attack(AttackPayloadId::BeeUltimateBomb)
        )));
    }

    #[test]
    fn bee_legacy_ultimate_ids_preserve_archived_sequence() {
        let catalog = CharacterMoveCatalog::default();
        let bee = LoadoutContext::for_character(
            CharacterKind::Bee,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let legacy_startup =
            technique_definition_by_id(TechniqueId::BeeLegacyUltimateStartup).unwrap();
        let legacy_rush = technique_definition_by_id(TechniqueId::BeeLegacyUltimateRush).unwrap();
        let startup_skill_events: Vec<_> = legacy_startup
            .script
            .events
            .iter()
            .filter_map(|event| match event.kind {
                MoveTimelineEventKind::SpawnBeeSkill(skill) => Some((event.at_ms, skill)),
                _ => None,
            })
            .collect();

        assert!(
            !catalog.allows_technique(CharacterKind::Bee, TechniqueId::BeeLegacyUltimateStartup)
        );
        assert!(!catalog.allows_technique(CharacterKind::Bee, TechniqueId::BeeLegacyUltimateRush));
        assert!(
            technique_definition_for_loadout_id_in_catalog(
                TechniqueId::BeeLegacyUltimateStartup,
                bee,
                &catalog
            )
            .is_none()
        );
        assert_eq!(legacy_startup.action, FighterAction::UltimateStartup);
        assert_eq!(legacy_rush.action, FighterAction::UltimateRush);
        assert_eq!(
            startup_skill_events,
            vec![
                (110, BeeSkillId::WorkerSwarm),
                (220, BeeSkillId::HomingSting),
                (360, BeeSkillId::WorkerSwarm),
                (500, BeeSkillId::HoneyGlob),
                (620, BeeSkillId::HomingSting),
            ]
        );
        assert!(legacy_startup.script.events.iter().any(|event| matches!(
            event.kind,
            MoveTimelineEventKind::Attack(AttackPayloadId::BeeUltimateBomb)
        )));
        assert!(legacy_rush.script.events.iter().any(|event| matches!(
            event.kind,
            MoveTimelineEventKind::Attack(AttackPayloadId::BeeUltimateScratchLight)
        )));
    }

    #[test]
    fn pig_first_two_c_attacks_match_cat_timing_and_chain_delay() {
        let catalog = CharacterMoveCatalog::default();
        let pig = LoadoutContext::for_character(
            CharacterKind::Pig,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let cat = LoadoutContext::for_character(
            CharacterKind::Cat,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let cat_light1 =
            technique_definition_for_loadout_id_in_catalog(TechniqueId::CatLight1, cat, &catalog)
                .unwrap();
        let cat_light2 =
            technique_definition_for_loadout_id_in_catalog(TechniqueId::CatLight2, cat, &catalog)
                .unwrap();
        let pig_light1 =
            technique_definition_for_loadout_id_in_catalog(TechniqueId::PigLight1, pig, &catalog)
                .unwrap();
        let pig_light2 =
            technique_definition_for_loadout_id_in_catalog(TechniqueId::PigLight2, pig, &catalog)
                .unwrap();

        assert_eq!(
            timeline_timing_signature(&pig_light1, None),
            timeline_timing_signature(&cat_light1, None)
        );
        assert_eq!(
            timeline_timing_signature(&pig_light2, None),
            timeline_timing_signature(&cat_light2, None)
        );
        assert_eq!(
            pig_light1.script.animation_recovery_ms,
            cat_light1.script.animation_recovery_ms
        );
        assert_eq!(
            pig_light1.script.next_tech_ms,
            cat_light1.script.next_tech_ms
        );
        assert_eq!(pig_light1.script.recover_ms, cat_light1.script.recover_ms);
        assert_eq!(pig_light1.cancel_window, cat_light1.cancel_window);
        assert_eq!(pig_light1.branch_window, cat_light1.branch_window);
        assert_eq!(
            pig_light2.script.animation_recovery_ms,
            cat_light2.script.animation_recovery_ms
        );
        assert_eq!(
            pig_light2.script.next_tech_ms,
            cat_light2.script.next_tech_ms
        );
        assert_eq!(pig_light2.script.recover_ms, cat_light2.script.recover_ms);
        assert_eq!(pig_light2.cancel_window, cat_light2.cancel_window);
        assert_eq!(pig_light2.branch_window, cat_light2.branch_window);
        assert_eq!(
            pig_light2.chain_rule.unwrap().window,
            cat_light2.chain_rule.unwrap().window
        );

        let feel = crate::feel::CombatFeelTuning::default();
        let cat_light1 = feel.apply_technique(cat_light1);
        let cat_light2 = feel.apply_technique(cat_light2);
        let pig_light1 = feel.apply_technique(pig_light1);
        let pig_light2 = feel.apply_technique(pig_light2);

        assert_eq!(
            timeline_timing_signature(&pig_light1, Some(&feel)),
            timeline_timing_signature(&cat_light1, Some(&feel))
        );
        assert_eq!(
            timeline_timing_signature(&pig_light2, Some(&feel)),
            timeline_timing_signature(&cat_light2, Some(&feel))
        );
        assert_eq!(
            pig_light1.script.animation_recovery_ms,
            cat_light1.script.animation_recovery_ms
        );
        assert_eq!(
            pig_light1.script.next_tech_ms,
            cat_light1.script.next_tech_ms
        );
        assert_eq!(pig_light1.script.recover_ms, cat_light1.script.recover_ms);
        assert_eq!(pig_light1.input_buffer_ms, cat_light1.input_buffer_ms);
        assert_eq!(pig_light1.cancel_window, cat_light1.cancel_window);
        assert_eq!(pig_light1.branch_window, cat_light1.branch_window);
        assert_eq!(
            pig_light2.script.animation_recovery_ms,
            cat_light2.script.animation_recovery_ms
        );
        assert_eq!(
            pig_light2.script.next_tech_ms,
            cat_light2.script.next_tech_ms
        );
        assert_eq!(pig_light2.script.recover_ms, cat_light2.script.recover_ms);
        assert_eq!(pig_light2.input_buffer_ms, cat_light2.input_buffer_ms);
        assert_eq!(pig_light2.cancel_window, cat_light2.cancel_window);
        assert_eq!(pig_light2.branch_window, cat_light2.branch_window);
    }

    #[test]
    fn pig_ultimate_is_long_startup_unblockable_grab() {
        let catalog = CharacterMoveCatalog::default();
        let pig = LoadoutContext::for_character(
            CharacterKind::Pig,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let startup =
            technique_slot_for_loadout(CharacterMoveSlot::UltimateStartup, pig, &catalog).unwrap();
        let catch_at = startup
            .script
            .events
            .iter()
            .find_map(|event| match event.kind {
                MoveTimelineEventKind::Attack(AttackPayloadId::PigUltimateCatch) => {
                    Some(event.at_ms)
                }
                _ => None,
            })
            .unwrap();
        let catch = attack_payload_definition(AttackPayloadId::PigUltimateCatch);

        assert_eq!(catch_at, 980);
        assert_eq!(startup.duration_ms(), 2200);
        assert!(!catch.guardable);
        assert_eq!(catch.shape_id, AttackShapeId::PigUltimateGrab);
    }

    #[test]
    fn x_string_uses_short_body_follow_timings() {
        let step = technique_definition_by_id(TechniqueId::CatHeavy).unwrap();
        let step_attack_times: Vec<u32> = step
            .script
            .events
            .iter()
            .filter_map(|event| match event.kind {
                MoveTimelineEventKind::Attack(_) => Some(event.at_ms),
                _ => None,
            })
            .collect();
        let step_motion_times: Vec<(u32, f32)> = step
            .script
            .events
            .iter()
            .filter_map(|event| match event.kind {
                MoveTimelineEventKind::Motion { forward, .. } => Some((event.at_ms, forward)),
                _ => None,
            })
            .collect();
        assert_eq!(step.id, TechniqueId::CatHeavy);
        assert_eq!(step.script.id, "heavy_step.sc");
        assert_eq!(step_motion_times, vec![(80, 5.4)]);
        assert_eq!(step_attack_times, vec![180]);
        assert_eq!(step.script.next_tech_ms, Some(260));
        assert_eq!(step.script.recover_ms, 430);
        assert_eq!(step.input_buffer_ms, 320);
        assert_eq!(step.branch_window.unwrap(), MsTimingWindow::closed(0, 380));
        let step_payload = attack_payload_definition(AttackPayloadId::HeavyStep);
        assert!(step_payload.knockback > 0.0);
        assert!(step_payload.knockback < HEAVY_KNOCKBACK * 0.2);

        let technique = technique_definition_by_id(TechniqueId::CatHeavy2).unwrap();
        let attack_times: Vec<u32> = technique
            .script
            .events
            .iter()
            .filter_map(|event| match event.kind {
                MoveTimelineEventKind::Attack(_) => Some(event.at_ms),
                _ => None,
            })
            .collect();
        let payloads: Vec<AttackPayloadId> = technique
            .script
            .events
            .iter()
            .filter_map(|event| match event.kind {
                MoveTimelineEventKind::Attack(payload) => Some(payload),
                _ => None,
            })
            .collect();
        let motion_times: Vec<(u32, f32, f32)> = technique
            .script
            .events
            .iter()
            .filter_map(|event| match event.kind {
                MoveTimelineEventKind::Motion { forward, lift } => {
                    Some((event.at_ms, forward, lift))
                }
                _ => None,
            })
            .collect();
        assert_eq!(technique.id, TechniqueId::CatHeavy2);
        assert_eq!(technique.script.id, "kiriage.sc");
        assert_eq!(
            motion_times,
            vec![(90, 1.4, 0.0), (220, 0.8, 3.2), (380, 0.4, 0.0)]
        );
        assert_eq!(attack_times, vec![360]);
        assert_eq!(payloads, vec![AttackPayloadId::KiriageBeat2]);
        assert_eq!(
            technique.chain_rule.unwrap().window,
            MsTimingWindow::closed(0, 380)
        );
        assert_eq!(technique.script.animation_recovery_ms, Some(500));
        assert_eq!(technique.script.next_tech_ms, Some(650));
        assert!(
            technique
                .script
                .events
                .iter()
                .any(
                    |event| matches!(event.kind, MoveTimelineEventKind::Stop) && event.at_ms == 430
                )
        );
        assert_eq!(technique.script.recover_ms, 960);
    }

    #[test]
    fn x_x_chains_heavy_step_into_launcher() {
        assert_eq!(
            raw_technique_for_button(TechniqueButton::B, true, FighterStyleKind::Anchor)
                .unwrap()
                .id,
            TechniqueId::CatHeavy
        );
        assert_eq!(
            chained_technique_for_context(TechniqueMatchContext {
                previous: Some(TechniqueId::CatHeavy),
                button: TechniqueButton::B,
                elapsed: 0.3,
                style: FighterStyleKind::Anchor,
                loadout: LoadoutContext::new(FighterStyleKind::Anchor, EquipmentKind::CounterCell),
                grounded: true,
                confirmed_hit: false,
                cancel_window_open: true,
                branch_window_open: true,
                current_action: FighterAction::HeavyAttack,
            })
            .unwrap()
            .id,
            TechniqueId::CatHeavy2
        );
    }

    #[test]
    fn payload_shape_and_reaction_are_separate_layers() {
        let payload = attack_payload_definition(AttackPayloadId::KiriageBeat2);
        let shape = attack_shape_definition(payload.shape_id);

        assert_eq!(payload.reaction_family, ReactionFamilyId::LauncherDown);
        assert_eq!(shape.id, AttackShapeId::RisingColumn);
        assert!(shape.path.len() > 1);
        assert_eq!(payload.time_ms, 100);
    }

    #[test]
    fn confirmed_a_ss_can_branch_to_authored_finisher() {
        assert_eq!(
            chained_technique_for_context(TechniqueMatchContext {
                previous: Some(TechniqueId::CatLight2),
                button: TechniqueButton::A,
                elapsed: 0.4,
                style: FighterStyleKind::Anchor,
                loadout: LoadoutContext::new(FighterStyleKind::Anchor, EquipmentKind::CounterCell),
                grounded: true,
                confirmed_hit: true,
                cancel_window_open: true,
                branch_window_open: true,
                current_action: FighterAction::LightAttack2,
            })
            .unwrap()
            .id,
            TechniqueId::CatComboFinisher
        );
        assert_eq!(
            chained_technique_for_context(TechniqueMatchContext {
                previous: Some(TechniqueId::CatLight2),
                button: TechniqueButton::A,
                elapsed: 0.4,
                style: FighterStyleKind::Anchor,
                loadout: LoadoutContext::new(FighterStyleKind::Anchor, EquipmentKind::CounterCell),
                grounded: true,
                confirmed_hit: false,
                cancel_window_open: true,
                branch_window_open: true,
                current_action: FighterAction::LightAttack2,
            })
            .unwrap()
            .id,
            TechniqueId::CatComboFinisher
        );
    }

    #[test]
    fn predicate_sets_support_all_any_and_none_groups() {
        const ALL: &[TechniquePredicate] = &[
            TechniquePredicate::Button(TechniqueButton::A),
            TechniquePredicate::Grounded,
        ];
        const ANY: &[TechniquePredicate] = &[
            TechniquePredicate::ConfirmedHit,
            TechniquePredicate::Style(FighterStyleKind::Vector),
        ];
        const NONE: &[TechniquePredicate] = &[
            TechniquePredicate::Airborne,
            TechniquePredicate::CurrentAction(FighterAction::HeavyAttack),
        ];
        let set = TechniquePredicateSet {
            all: ALL,
            any: ANY,
            none: NONE,
        };

        assert!(set.matches(TechniqueMatchContext {
            previous: Some(TechniqueId::CatLight2),
            button: TechniqueButton::A,
            elapsed: 0.4,
            style: FighterStyleKind::Anchor,
            loadout: LoadoutContext::new(FighterStyleKind::Anchor, EquipmentKind::CounterCell),
            grounded: true,
            confirmed_hit: true,
            cancel_window_open: true,
            branch_window_open: true,
            current_action: FighterAction::LightAttack2,
        }));
        assert!(set.matches(TechniqueMatchContext {
            previous: Some(TechniqueId::CatLight2),
            button: TechniqueButton::A,
            elapsed: 0.4,
            style: FighterStyleKind::Vector,
            loadout: LoadoutContext::new(FighterStyleKind::Vector, EquipmentKind::CounterCell),
            grounded: true,
            confirmed_hit: false,
            cancel_window_open: true,
            branch_window_open: true,
            current_action: FighterAction::LightAttack2,
        }));
        assert!(!set.matches(TechniqueMatchContext {
            previous: Some(TechniqueId::CatLight2),
            button: TechniqueButton::A,
            elapsed: 0.4,
            style: FighterStyleKind::Vector,
            loadout: LoadoutContext::new(FighterStyleKind::Vector, EquipmentKind::CounterCell),
            grounded: true,
            confirmed_hit: true,
            cancel_window_open: true,
            branch_window_open: true,
            current_action: FighterAction::HeavyAttack,
        }));
    }

    #[test]
    fn technique_predicates_can_match_loadout_data() {
        const ALL: &[TechniquePredicate] = &[
            TechniquePredicate::Equipment(EquipmentKind::DashCoil),
            TechniquePredicate::LoadoutTag(LoadoutTag::DashFlow),
        ];
        let set = TechniquePredicateSet {
            all: ALL,
            any: NO_TECHNIQUE_PREDICATES,
            none: NO_TECHNIQUE_PREDICATES,
        };
        let context = TechniqueMatchContext {
            previous: Some(TechniqueId::CatDashAttack),
            button: TechniqueButton::A,
            elapsed: 0.2,
            style: FighterStyleKind::Vector,
            loadout: LoadoutContext::new(FighterStyleKind::Vector, EquipmentKind::DashCoil),
            grounded: true,
            confirmed_hit: false,
            cancel_window_open: true,
            branch_window_open: true,
            current_action: FighterAction::DashAttack,
        };

        assert!(set.matches(context));
    }

    #[test]
    fn aoa_inspired_shape_atlas_covers_static_riser_thrust_and_curve() {
        assert_eq!(
            attack_shape_definition(AttackShapeId::BodyRoll).path.len(),
            1
        );
        assert_eq!(
            attack_shape_definition(AttackShapeId::CompactThrust).effect_type,
            6
        );
        assert_eq!(
            attack_shape_definition(AttackShapeId::DelayedRiser)
                .path
                .len(),
            11
        );
        assert_eq!(
            attack_shape_definition(AttackShapeId::RisingColumn)
                .path
                .len(),
            8
        );
        assert!(attack_shape_definition(AttackShapeId::SweepingArcWide).curved);
        assert!(attack_shape_definition(AttackShapeId::HookSweep).curved);
        assert!(attack_shape_definition(AttackShapeId::FallingSpikeArc).curved);
        assert!(!attack_shape_definition(AttackShapeId::AirFishShot).parented);
        assert!(attack_shape_definition(AttackShapeId::CurvedLob).curved);
        assert!(attack_shape_definition(AttackShapeId::PigBellyCrash).parented);
        assert!(attack_shape_definition(AttackShapeId::PigHamLob).curved);
    }

    #[test]
    fn payloads_reuse_broader_shape_atlas_paths() {
        assert_eq!(
            attack_payload_definition(AttackPayloadId::AssBeat1).shape_id,
            AttackShapeId::CompactSlashFollow
        );
        assert_eq!(
            attack_payload_definition(AttackPayloadId::AssBeat2).shape_id,
            AttackShapeId::HookSweep
        );
        assert_eq!(
            attack_payload_definition(AttackPayloadId::DashShoulderBeat).shape_id,
            AttackShapeId::ShoulderLine
        );
        assert_eq!(
            attack_payload_definition(AttackPayloadId::JumpSpike).shape_id,
            AttackShapeId::FallingSpikeArc
        );
        assert_eq!(
            attack_payload_definition(AttackPayloadId::JumpFishShot).shape_id,
            AttackShapeId::AirFishShot
        );
        assert_eq!(
            attack_payload_definition(AttackPayloadId::ComboFinisher).shape_id,
            AttackShapeId::CatBodySkid
        );
        assert_eq!(
            attack_payload_definition(AttackPayloadId::PigRollingPinStep).shape_id,
            AttackShapeId::PigRollingPinLine
        );
        assert_eq!(
            attack_payload_definition(AttackPayloadId::PigHamLob).shape_id,
            AttackShapeId::PigHamLob
        );
    }

    #[test]
    fn cat_combo_finisher_hitbox_follows_body_motion() {
        let technique = technique_definition_by_id(TechniqueId::CatComboFinisher).unwrap();
        let motion_events: Vec<(u32, f32, f32)> = technique
            .script
            .events
            .iter()
            .filter_map(|event| match event.kind {
                MoveTimelineEventKind::Motion { forward, lift } => {
                    Some((event.at_ms, forward, lift))
                }
                _ => None,
            })
            .collect();
        let attack_time = technique
            .script
            .events
            .iter()
            .find_map(|event| match event.kind {
                MoveTimelineEventKind::Attack(AttackPayloadId::ComboFinisher) => Some(event.at_ms),
                _ => None,
            })
            .unwrap();
        let shape = attack_shape_definition(AttackShapeId::CatBodySkid);
        let detached_skid = attack_shape_definition(AttackShapeId::GroundSkid);

        assert_eq!(
            motion_events,
            vec![(120, 7.0, 0.0), (250, 5.0, 0.0), (330, 4.0, 0.0)]
        );
        assert!(motion_events[0].0 < attack_time);
        assert!(motion_events[1].0 < attack_time);
        assert!(motion_events[2].0 > attack_time);
        assert!(
            technique
                .script
                .events
                .iter()
                .any(
                    |event| matches!(event.kind, MoveTimelineEventKind::Stop) && event.at_ms == 430
                )
        );
        assert_eq!(technique.script.recover_ms, 900);
        assert_eq!(
            attack_payload_definition(AttackPayloadId::ComboFinisher).shape_id,
            AttackShapeId::CatBodySkid
        );
        assert!(shape.parented);
        assert!(shape.range <= FIGHTER_RADIUS);
        assert!(shape.radius < COMBO_FINISHER_RADIUS * 0.6);
        assert!(CAT_BODY_SKID_PATH[3][2] - CAT_BODY_SKID_PATH[0][2] < FIGHTER_RADIUS * 0.6);
        assert!(!detached_skid.parented);
        assert!(GROUND_SKID_PATH[3][2] > CAT_BODY_SKID_PATH[3][2]);
    }

    #[test]
    fn cat_dash_combo_finisher_starts_hitbox_with_body_launch() {
        let catalog = CharacterMoveCatalog::default();
        let loadout = LoadoutContext::from_style(FighterStyleKind::Anchor);
        let penguin = LoadoutContext::for_character(
            CharacterKind::Penguin,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let dash_finisher =
            technique_slot_for_loadout(CharacterMoveSlot::DashLight, loadout, &catalog).unwrap();
        let penguin_dash_x =
            technique_slot_for_loadout(CharacterMoveSlot::DashHeavy, penguin, &catalog).unwrap();
        let grounded_finisher = technique_definition_by_id(TechniqueId::CatComboFinisher).unwrap();
        let dash_attack_time = dash_finisher
            .script
            .events
            .iter()
            .find_map(|event| match event.kind {
                MoveTimelineEventKind::Attack(AttackPayloadId::DashComboFinisher) => {
                    Some(event.at_ms)
                }
                _ => None,
            })
            .unwrap();
        let penguin_attack_time = penguin_dash_x
            .script
            .events
            .iter()
            .find_map(|event| match event.kind {
                MoveTimelineEventKind::SpawnPenguinSkill(PenguinSkillId::SnowflakeShot) => {
                    Some(event.at_ms)
                }
                _ => None,
            })
            .unwrap();
        let grounded_attack_time = grounded_finisher
            .script
            .events
            .iter()
            .find_map(|event| match event.kind {
                MoveTimelineEventKind::Attack(AttackPayloadId::ComboFinisher) => Some(event.at_ms),
                _ => None,
            })
            .unwrap();
        let first_dash_motion = dash_finisher
            .script
            .events
            .iter()
            .find_map(|event| match event.kind {
                MoveTimelineEventKind::Motion { forward, lift } => {
                    Some((event.at_ms, forward, lift))
                }
                _ => None,
            })
            .unwrap();

        assert_eq!(dash_finisher.id, TechniqueId::CatDashComboFinisher);
        assert_eq!(dash_finisher.action, FighterAction::ComboFinisher);
        assert_eq!(dash_attack_time, 0);
        assert_eq!(penguin_dash_x.id, TechniqueId::PenguinDashHeavy);
        assert_eq!(penguin_attack_time, 0);
        assert_eq!(first_dash_motion, (0, 7.0, 0.0));
        assert_eq!(grounded_attack_time, 310);
        assert_eq!(
            attack_payload_definition(AttackPayloadId::DashComboFinisher).shape_id,
            AttackShapeId::CatBodySkid
        );
        assert_eq!(penguin_dash_x.script.recover_ms, 180);
        assert!(attack_shape_definition(AttackShapeId::CatBodySkid).parented);
        assert_eq!(
            attack_payload_definition(AttackPayloadId::DashComboFinisher).time_ms,
            430
        );
        assert_eq!(
            attack_payload_definition(AttackPayloadId::ComboFinisher).time_ms,
            100
        );
    }

    #[test]
    fn dash_attack_is_authored_as_a_multi_beat_slice() {
        let technique = technique_definition_by_id(TechniqueId::CatDashAttack).unwrap();
        let attack_times: Vec<u32> = technique
            .script
            .events
            .iter()
            .filter_map(|event| match event.kind {
                MoveTimelineEventKind::Attack(_) => Some(event.at_ms),
                _ => None,
            })
            .collect();

        assert_eq!(attack_times, vec![160, 230]);
        assert_eq!(technique.script.next_tech_ms, Some(500));
        assert_eq!(
            attack_payload_definition(AttackPayloadId::DashShoulderBeat).reaction_family,
            ReactionFamilyId::LightAirPop
        );
    }

    #[test]
    fn jump_attack_uses_spike_aftermath_payload() {
        let technique = technique_definition_by_id(TechniqueId::CatJumpAttack).unwrap();
        let payload = attack_payload_definition(AttackPayloadId::JumpSpike);
        let shape = attack_shape_definition(AttackShapeId::FallingSpikeArc);

        assert!(technique.script.events.iter().any(|event| matches!(
            event.kind,
            MoveTimelineEventKind::Attack(AttackPayloadId::JumpSpike)
        )));
        assert!(technique.script.events.iter().any(|event| matches!(
            event.kind,
            MoveTimelineEventKind::Attack(AttackPayloadId::JumpSpike) if event.at_ms == 0
        )));
        assert!(!technique.script.events.iter().any(|event| matches!(
            event.kind,
            MoveTimelineEventKind::Attack(AttackPayloadId::JumpStrike)
        )));
        assert_eq!(payload.reaction_family, ReactionFamilyId::AerialSpikeDown);
        assert_eq!(payload.time_ms, (JUMP_ATTACK_MAX_ACTIVE * 1000.0) as u32);
        assert_eq!(shape.range, LIGHT_RANGE);
        assert!(shape.radius > LIGHT_RADIUS);
        assert!(payload.hitstop_scale > 1.0);
    }

    #[test]
    fn jump_heavy_fires_shallow_air_fish_knockdown() {
        let technique = technique_definition_by_id(TechniqueId::CatJumpHeavy).unwrap();
        let payload = attack_payload_definition(AttackPayloadId::JumpFishShot);
        let shape = attack_shape_definition(AttackShapeId::AirFishShot);
        let raw =
            raw_technique_for_button(TechniqueButton::B, false, FighterStyleKind::Anchor).unwrap();
        let fish_drop = AIR_FISH_SHOT_PATH[0][1] - AIR_FISH_SHOT_PATH[6][1];
        let fish_forward = AIR_FISH_SHOT_PATH[6][2] - AIR_FISH_SHOT_PATH[0][2];
        let spike_drop = FALLING_SPIKE_ARC_PATH[0][1] - FALLING_SPIKE_ARC_PATH[5][1];
        let spike_forward = FALLING_SPIKE_ARC_PATH[5][2] - FALLING_SPIKE_ARC_PATH[0][2];

        assert_eq!(raw.id, TechniqueId::CatJumpHeavy);
        assert_eq!(technique.button, TechniqueButton::B);
        assert_eq!(technique.status, TechniqueStatus::Airborne);
        assert!(technique.script.events.iter().any(|event| matches!(
            event.kind,
            MoveTimelineEventKind::Attack(AttackPayloadId::JumpFishShot) if event.at_ms == 90
        )));
        assert_eq!(payload.kind, AttackKind::Heavy);
        assert_eq!(payload.reaction_family, ReactionFamilyId::AirFishKnockdown);
        assert_eq!(payload.damage_profile, DamageProfileId::GroundBounce);
        assert!(!shape.parented);
        assert!((fish_forward - 4.6 * 0.8).abs() < 0.001);
        assert!(fish_drop / fish_forward < spike_drop / spike_forward);
    }

    #[test]
    fn ultimate_uses_confirm_startup_then_locked_rush_payloads() {
        let startup = technique_definition_by_id(TechniqueId::CatUltimateStartup).unwrap();
        let rush = technique_definition_by_id(TechniqueId::CatUltimateRush).unwrap();
        let raw =
            raw_technique_for_button(TechniqueButton::Ultimate, true, FighterStyleKind::Anchor)
                .unwrap();
        let rush_payloads: Vec<_> = rush
            .script
            .events
            .iter()
            .filter_map(|event| match event.kind {
                MoveTimelineEventKind::Attack(payload) => Some(payload),
                _ => None,
            })
            .collect();
        let startup_motions: Vec<_> = startup
            .script
            .events
            .iter()
            .filter_map(|event| match event.kind {
                MoveTimelineEventKind::Motion { forward, lift } => {
                    Some((event.at_ms, forward, lift))
                }
                _ => None,
            })
            .collect();
        let total_damage: f32 = [
            AttackPayloadId::UltimateCatch,
            AttackPayloadId::UltimateScratchLight,
            AttackPayloadId::UltimateScratchLight,
            AttackPayloadId::UltimateScratchHeavy,
            AttackPayloadId::UltimateScratchHeavy,
            AttackPayloadId::UltimateBomb,
        ]
        .into_iter()
        .map(|payload| attack_payload_definition(payload).damage)
        .sum();

        assert_eq!(raw.id, TechniqueId::CatUltimateStartup);
        assert_eq!(startup.button, TechniqueButton::Ultimate);
        assert_eq!(startup.stamina_cost, ULTIMATE_STAMINA_COST);
        assert!(startup.script.events.iter().any(|event| matches!(
            event.kind,
            MoveTimelineEventKind::Attack(AttackPayloadId::UltimateCatch) if event.at_ms == 80
        )));
        assert_eq!(
            startup_motions,
            vec![(80, 14.0, 0.0), (180, 10.0, 0.0), (300, 8.0, 0.0)]
        );
        assert_eq!(
            rush_payloads,
            vec![
                AttackPayloadId::UltimateScratchLight,
                AttackPayloadId::UltimateScratchLight,
                AttackPayloadId::UltimateScratchHeavy,
                AttackPayloadId::UltimateScratchHeavy,
                AttackPayloadId::UltimateBomb,
            ]
        );
        assert_eq!(total_damage, 40.0);
        assert_eq!(
            attack_payload_definition(AttackPayloadId::UltimateBomb).reaction_family,
            ReactionFamilyId::UltimateBombDown
        );
    }

    #[test]
    fn cat_ultimate_startup_uses_body_following_pounce_catch() {
        let payload = attack_payload_definition(AttackPayloadId::UltimateCatch);
        let shape = attack_shape_definition(payload.shape_id);

        assert_eq!(payload.shape_id, AttackShapeId::CatPounceCatch);
        assert_eq!(payload.time_ms, 350);
        assert!(shape.parented);
        assert_eq!(shape.path, CAT_POUNCE_CATCH_PATH.as_slice());
        assert!(shape.range < FIGHTER_RADIUS);
        assert!(shape.radius <= FIGHTER_RADIUS);

        for payload in [
            AttackPayloadId::DogUltimateCatch,
            AttackPayloadId::FoxUltimateCatch,
            AttackPayloadId::PandaUltimateCatch,
            AttackPayloadId::BeeUltimateCatch,
            AttackPayloadId::PenguinUltimateCatch,
        ] {
            assert_eq!(
                attack_payload_definition(payload).shape_id,
                AttackShapeId::UltimateCatch
            );
        }
        assert_eq!(
            attack_payload_definition(AttackPayloadId::PigUltimateCatch).shape_id,
            AttackShapeId::PigUltimateGrab
        );
    }

    #[test]
    fn ultimate_scratch_trails_are_authored_before_each_scratch_hit() {
        let rush = technique_definition_by_id(TechniqueId::CatUltimateRush).unwrap();
        let mut pending_trail_at = None;
        let mut scratch_hits = 0;

        for event in rush.script.events {
            match event.kind {
                MoveTimelineEventKind::Feedback(_, "trail_ultimate_scratch") => {
                    pending_trail_at = Some(event.at_ms);
                }
                MoveTimelineEventKind::Attack(
                    AttackPayloadId::UltimateScratchLight | AttackPayloadId::UltimateScratchHeavy,
                ) => {
                    let trail_at = pending_trail_at.take().expect("missing scratch trail cue");
                    assert!(trail_at < event.at_ms);
                    assert!(event.at_ms - trail_at <= 30);
                    scratch_hits += 1;
                }
                _ => {}
            }
        }

        assert_eq!(scratch_hits, 4);
    }

    #[test]
    fn utility_states_are_authored_timeline_packages() {
        let guard_step = technique_definition(FighterAction::GuardStep).unwrap();
        assert_eq!(guard_step.script.id, "guard_step.sc");
        assert_eq!(guard_step.script.recover_ms, 260);
        assert!(
            guard_step
                .script
                .events
                .iter()
                .any(|event| matches!(event.kind, MoveTimelineEventKind::Motion { .. }))
        );

        let pickup = technique_definition(FighterAction::ItemPickup).unwrap();
        assert_eq!(pickup.script.animation_recovery_ms, Some(150));
        assert!(pickup.script.events.iter().any(|event| matches!(
            event.kind,
            MoveTimelineEventKind::Feedback(FeedbackPhase::Aftermath, "secure_item_pickup")
        )));

        let landing = technique_definition(FighterAction::LandingRecovery).unwrap();
        assert_eq!(landing.script.recover_ms, 220);
        assert!(landing.script.events.iter().any(|event| matches!(
            event.kind,
            MoveTimelineEventKind::Feedback(FeedbackPhase::Startup, "landing_recovery_stick")
        )));
    }

    #[test]
    fn style_timing_keeps_vector_dash_flow_shorter() {
        let catalog = CharacterMoveCatalog::default();
        let anchor = technique_definition_for_loadout_in_catalog(
            FighterAction::DashAttack,
            LoadoutContext::from_style(FighterStyleKind::Anchor),
            &catalog,
        )
        .unwrap();
        let vector = technique_definition_for_loadout_in_catalog(
            FighterAction::DashAttack,
            LoadoutContext::from_style(FighterStyleKind::Vector),
            &catalog,
        )
        .unwrap();
        assert!(vector.duration() < anchor.duration());
    }

    #[test]
    fn vector_dash_attack_has_light_branch_window() {
        let catalog = CharacterMoveCatalog::default();
        assert!(
            technique_runtime_for_loadout_in_catalog(
                FighterAction::DashAttack,
                0.2,
                LoadoutContext::from_style(FighterStyleKind::Vector),
                &catalog,
            )
            .branch_open
        );
        assert!(
            !technique_runtime_for_loadout_in_catalog(
                FighterAction::DashAttack,
                0.2,
                LoadoutContext::from_style(FighterStyleKind::Anchor),
                &catalog,
            )
            .branch_open
        );
    }
}
