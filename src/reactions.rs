#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReactionKind {
    Hitstun,
    Launch,
    Tumble,
    GroundBounce,
    WallBounce,
    HardKnockdown,
    LandingRecovery,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
pub enum ReactionFamilyId {
    ShortStandingStagger,
    MediumStandingStagger,
    HeavyStandingStagger,
    FrozenStun,
    LauncherDown,
    GroundedDownGetup,
    SlidingKnockdown,
    LightAirPop,
    CounterPop,
    GroundBounceDown,
    AerialSpikeDown,
    AirFishKnockdown,
    UltimateLockedStagger,
    UltimateBombDown,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct QueuedAftermath {
    pub family: ReactionFamilyId,
    pub getup_transition_ms: u32,
    pub recover_ms: u32,
    pub landing_stick_ms: u32,
    pub horizontal_damping: f32,
    pub cue: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReactionFamilyDef {
    pub id: ReactionFamilyId,
    pub kind: ReactionKind,
    pub horizontal_scale: f32,
    pub vertical_scale: f32,
    pub airborne: bool,
    pub immediate_down: bool,
    pub hitstun_recover_ms: Option<u32>,
    pub grounded_getup_ms: Option<u32>,
    pub grounded_recover_ms: Option<u32>,
    pub grounded_stick_ms: u32,
    pub landing_aftermath: Option<QueuedAftermath>,
    pub cue: &'static str,
    pub priority_bonus: u8,
}

pub type ReactionProfile = ReactionFamilyDef;

pub fn reaction_family_definition(id: ReactionFamilyId) -> ReactionFamilyDef {
    match id {
        ReactionFamilyId::ShortStandingStagger => ReactionFamilyDef {
            id,
            kind: ReactionKind::Hitstun,
            horizontal_scale: 0.58,
            vertical_scale: 0.0,
            airborne: false,
            immediate_down: false,
            hitstun_recover_ms: Some(500),
            grounded_getup_ms: None,
            grounded_recover_ms: None,
            grounded_stick_ms: 0,
            landing_aftermath: None,
            cue: "reaction_short_stagger",
            priority_bonus: 2,
        },
        ReactionFamilyId::MediumStandingStagger => ReactionFamilyDef {
            id,
            kind: ReactionKind::Hitstun,
            horizontal_scale: 0.72,
            vertical_scale: 0.0,
            airborne: false,
            immediate_down: false,
            hitstun_recover_ms: Some(620),
            grounded_getup_ms: None,
            grounded_recover_ms: None,
            grounded_stick_ms: 0,
            landing_aftermath: None,
            cue: "reaction_medium_stagger",
            priority_bonus: 4,
        },
        ReactionFamilyId::HeavyStandingStagger => ReactionFamilyDef {
            id,
            kind: ReactionKind::Hitstun,
            horizontal_scale: 0.88,
            vertical_scale: 0.0,
            airborne: false,
            immediate_down: false,
            hitstun_recover_ms: Some(760),
            grounded_getup_ms: None,
            grounded_recover_ms: None,
            grounded_stick_ms: 0,
            landing_aftermath: None,
            cue: "reaction_heavy_stagger",
            priority_bonus: 7,
        },
        ReactionFamilyId::FrozenStun => ReactionFamilyDef {
            id,
            kind: ReactionKind::Hitstun,
            horizontal_scale: 0.0,
            vertical_scale: 0.0,
            airborne: false,
            immediate_down: false,
            hitstun_recover_ms: Some(750),
            grounded_getup_ms: None,
            grounded_recover_ms: None,
            grounded_stick_ms: 0,
            landing_aftermath: None,
            cue: "reaction_frozen_stun",
            priority_bonus: 8,
        },
        ReactionFamilyId::LauncherDown => ReactionFamilyDef {
            id,
            kind: ReactionKind::Launch,
            horizontal_scale: 1.0,
            vertical_scale: 1.0,
            airborne: true,
            immediate_down: false,
            hitstun_recover_ms: None,
            grounded_getup_ms: None,
            grounded_recover_ms: None,
            grounded_stick_ms: 0,
            landing_aftermath: Some(QueuedAftermath {
                family: ReactionFamilyId::GroundedDownGetup,
                getup_transition_ms: 500,
                recover_ms: 700,
                landing_stick_ms: 120,
                horizontal_damping: 0.42,
                cue: "reaction_down_getup",
            }),
            cue: "reaction_launcher_down",
            priority_bonus: 12,
        },
        ReactionFamilyId::GroundedDownGetup => ReactionFamilyDef {
            id,
            kind: ReactionKind::HardKnockdown,
            horizontal_scale: 0.72,
            vertical_scale: 0.35,
            airborne: false,
            immediate_down: true,
            hitstun_recover_ms: None,
            grounded_getup_ms: Some(500),
            grounded_recover_ms: Some(700),
            grounded_stick_ms: 100,
            landing_aftermath: None,
            cue: "reaction_grounded_down",
            priority_bonus: 10,
        },
        ReactionFamilyId::SlidingKnockdown => ReactionFamilyDef {
            id,
            kind: ReactionKind::HardKnockdown,
            horizontal_scale: 0.94,
            vertical_scale: 0.0,
            airborne: false,
            immediate_down: true,
            hitstun_recover_ms: None,
            grounded_getup_ms: Some(420),
            grounded_recover_ms: Some(760),
            grounded_stick_ms: 80,
            landing_aftermath: None,
            cue: "reaction_sliding_down",
            priority_bonus: 11,
        },
        ReactionFamilyId::LightAirPop => ReactionFamilyDef {
            id,
            kind: ReactionKind::Launch,
            horizontal_scale: 0.78,
            vertical_scale: 0.72,
            airborne: true,
            immediate_down: false,
            hitstun_recover_ms: Some(500),
            grounded_getup_ms: None,
            grounded_recover_ms: None,
            grounded_stick_ms: 0,
            landing_aftermath: None,
            cue: "reaction_air_pop",
            priority_bonus: 6,
        },
        ReactionFamilyId::CounterPop => ReactionFamilyDef {
            id,
            kind: ReactionKind::Launch,
            horizontal_scale: 0.7,
            vertical_scale: 0.86,
            airborne: true,
            immediate_down: false,
            hitstun_recover_ms: Some(640),
            grounded_getup_ms: None,
            grounded_recover_ms: None,
            grounded_stick_ms: 0,
            landing_aftermath: None,
            cue: "reaction_counter_pop",
            priority_bonus: 9,
        },
        ReactionFamilyId::GroundBounceDown => ReactionFamilyDef {
            id,
            kind: ReactionKind::GroundBounce,
            horizontal_scale: 0.78,
            vertical_scale: 0.94,
            airborne: true,
            immediate_down: false,
            hitstun_recover_ms: None,
            grounded_getup_ms: None,
            grounded_recover_ms: None,
            grounded_stick_ms: 0,
            landing_aftermath: Some(QueuedAftermath {
                family: ReactionFamilyId::GroundedDownGetup,
                getup_transition_ms: 620,
                recover_ms: 900,
                landing_stick_ms: 150,
                horizontal_damping: 0.36,
                cue: "reaction_bounce_down",
            }),
            cue: "reaction_ground_bounce",
            priority_bonus: 14,
        },
        ReactionFamilyId::AerialSpikeDown => ReactionFamilyDef {
            id,
            kind: ReactionKind::Tumble,
            horizontal_scale: 0.62,
            vertical_scale: -0.42,
            airborne: true,
            immediate_down: false,
            hitstun_recover_ms: None,
            grounded_getup_ms: None,
            grounded_recover_ms: None,
            grounded_stick_ms: 0,
            landing_aftermath: Some(QueuedAftermath {
                family: ReactionFamilyId::GroundedDownGetup,
                getup_transition_ms: 440,
                recover_ms: 680,
                landing_stick_ms: 110,
                horizontal_damping: 0.5,
                cue: "reaction_spike_down",
            }),
            cue: "reaction_aerial_spike",
            priority_bonus: 13,
        },
        ReactionFamilyId::AirFishKnockdown => ReactionFamilyDef {
            id,
            kind: ReactionKind::Tumble,
            horizontal_scale: 1.28,
            vertical_scale: 0.46,
            airborne: true,
            immediate_down: false,
            hitstun_recover_ms: None,
            grounded_getup_ms: None,
            grounded_recover_ms: None,
            grounded_stick_ms: 0,
            landing_aftermath: Some(QueuedAftermath {
                family: ReactionFamilyId::GroundedDownGetup,
                getup_transition_ms: 540,
                recover_ms: 820,
                landing_stick_ms: 140,
                horizontal_damping: 0.48,
                cue: "reaction_fish_knockdown",
            }),
            cue: "reaction_air_fish_knockdown",
            priority_bonus: 14,
        },
        ReactionFamilyId::UltimateLockedStagger => ReactionFamilyDef {
            id,
            kind: ReactionKind::Hitstun,
            horizontal_scale: 0.12,
            vertical_scale: 0.0,
            airborne: false,
            immediate_down: false,
            hitstun_recover_ms: Some(220),
            grounded_getup_ms: None,
            grounded_recover_ms: None,
            grounded_stick_ms: 0,
            landing_aftermath: None,
            cue: "reaction_ultimate_locked_stagger",
            priority_bonus: 9,
        },
        ReactionFamilyId::UltimateBombDown => ReactionFamilyDef {
            id,
            kind: ReactionKind::HardKnockdown,
            horizontal_scale: 1.05,
            vertical_scale: 0.66,
            airborne: true,
            immediate_down: false,
            hitstun_recover_ms: None,
            grounded_getup_ms: None,
            grounded_recover_ms: None,
            grounded_stick_ms: 0,
            landing_aftermath: Some(QueuedAftermath {
                family: ReactionFamilyId::GroundedDownGetup,
                getup_transition_ms: 660,
                recover_ms: 980,
                landing_stick_ms: 180,
                horizontal_damping: 0.32,
                cue: "reaction_ultimate_bomb_down",
            }),
            cue: "reaction_ultimate_bomb",
            priority_bonus: 18,
        },
    }
}

pub fn reaction_profile_for_family(id: ReactionFamilyId) -> ReactionProfile {
    reaction_family_definition(id)
}

#[allow(dead_code)]
pub fn ground_bounce_profile() -> ReactionProfile {
    ReactionFamilyDef {
        id: ReactionFamilyId::LauncherDown,
        kind: ReactionKind::GroundBounce,
        horizontal_scale: 0.72,
        vertical_scale: 0.82,
        airborne: true,
        immediate_down: false,
        hitstun_recover_ms: None,
        grounded_getup_ms: None,
        grounded_recover_ms: None,
        grounded_stick_ms: 0,
        landing_aftermath: reaction_family_definition(ReactionFamilyId::LauncherDown)
            .landing_aftermath,
        cue: "reaction_ground_bounce",
        priority_bonus: 12,
    }
}

#[allow(dead_code)]
pub fn wall_bounce_profile() -> ReactionProfile {
    ReactionFamilyDef {
        id: ReactionFamilyId::LauncherDown,
        kind: ReactionKind::WallBounce,
        horizontal_scale: 0.86,
        vertical_scale: 0.7,
        airborne: true,
        immediate_down: false,
        hitstun_recover_ms: None,
        grounded_getup_ms: None,
        grounded_recover_ms: None,
        grounded_stick_ms: 0,
        landing_aftermath: reaction_family_definition(ReactionFamilyId::LauncherDown)
            .landing_aftermath,
        cue: "reaction_wall_bounce",
        priority_bonus: 12,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_reaction_families_are_explicit() {
        let stagger = reaction_profile_for_family(ReactionFamilyId::ShortStandingStagger);
        let launcher = reaction_profile_for_family(ReactionFamilyId::LauncherDown);

        assert_eq!(stagger.kind, ReactionKind::Hitstun);
        assert!(!stagger.airborne);
        assert_eq!(stagger.hitstun_recover_ms, Some(500));
        assert_eq!(launcher.kind, ReactionKind::Launch);
        assert!(launcher.airborne);
        assert_eq!(launcher.landing_aftermath.unwrap().getup_transition_ms, 500);
        assert_eq!(launcher.landing_aftermath.unwrap().recover_ms, 700);
    }

    #[test]
    fn grounded_down_family_owns_floor_relative_recovery() {
        let down = reaction_profile_for_family(ReactionFamilyId::GroundedDownGetup);

        assert!(down.immediate_down);
        assert_eq!(down.grounded_getup_ms, Some(500));
        assert_eq!(down.cue, "reaction_grounded_down");
    }

    #[test]
    fn frozen_stun_is_short_grounded_immobilize() {
        let frozen = reaction_profile_for_family(ReactionFamilyId::FrozenStun);

        assert_eq!(frozen.kind, ReactionKind::Hitstun);
        assert_eq!(frozen.horizontal_scale, 0.0);
        assert_eq!(frozen.vertical_scale, 0.0);
        assert_eq!(frozen.hitstun_recover_ms, Some(750));
        assert!(!frozen.airborne);
        assert!(!frozen.immediate_down);
    }

    #[test]
    fn expanded_families_keep_distinct_aftermath_shapes() {
        let bounce = reaction_profile_for_family(ReactionFamilyId::GroundBounceDown);
        let spike = reaction_profile_for_family(ReactionFamilyId::AerialSpikeDown);
        let fish = reaction_profile_for_family(ReactionFamilyId::AirFishKnockdown);
        let slide = reaction_profile_for_family(ReactionFamilyId::SlidingKnockdown);

        assert_eq!(bounce.kind, ReactionKind::GroundBounce);
        assert!(bounce.landing_aftermath.unwrap().recover_ms > 800);
        assert_eq!(spike.kind, ReactionKind::Tumble);
        assert!(spike.vertical_scale < 0.0);
        assert_eq!(fish.kind, ReactionKind::Tumble);
        assert!(fish.horizontal_scale > bounce.horizontal_scale);
        assert!(fish.landing_aftermath.is_some());
        assert!(slide.immediate_down);
        assert_ne!(bounce.cue, spike.cue);
    }

    #[test]
    fn bounce_profiles_keep_distinct_debug_cues() {
        assert_eq!(ground_bounce_profile().kind, ReactionKind::GroundBounce);
        assert_eq!(wall_bounce_profile().kind, ReactionKind::WallBounce);
        assert_ne!(ground_bounce_profile().cue, wall_bounce_profile().cue);
    }
}
