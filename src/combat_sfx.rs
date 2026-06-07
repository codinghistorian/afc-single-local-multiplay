use bevy::audio::Volume;
use bevy::prelude::*;

use crate::combat::{
    HitEffects, ImpactFeedbackIntensity, ImpactProfile, ImpactSource, impact_feedback_profile,
};
use crate::reactions::ReactionFamilyId;
use crate::techniques::{
    AttackPayloadId, payload_is_ultimate_bomb, payload_is_ultimate_catch,
    payload_is_ultimate_scratch,
};

const LIGHT_HIT_SFX: &str = "soundeffects/hit/slap.wav";
const HEAVY_HIT_SFX: &str = "soundeffects/hit/punch_3.wav";
const ULTIMATE_HIT_SFX: &str = "soundeffects/hit/punch_retro.wav";
const ITEM_HIT_SFX: &str = "soundeffects/hit/kick.wav";
const GUARDED_SFX: &str = "soundeffects/hit/guarded.wav";
const PIG_ULTIMATE_MUNCH_SFX: &str = "soundeffects/hit/munching_food.wav";
const THROWN_ITEM_HIT_SFX: &str = "soundeffects/hit/item_hit_target.wav";
const PIG_JUMP_X_SFX: &str = "soundeffects/hit/pig_jump_x.wav";
const ITEM_DROP_SFX: &str = "soundeffects/hit/item_drop.wav";
const RESULT_WIN_SFX: &str = "soundeffects/hit/sound_when_won.wav";
const RESULT_LOSE_SFX: &str = "soundeffects/hit/sound_when_lost.wav";
const STEAMER_EXPLOSION_SFX: &str = "soundeffects/hit/steamer_explosion.wav";
const MUSHROOM_BIGGER_SFX: &str = "soundeffects/hit/mushroom_bigger_effect.wav";
const WALL_IMPACT_SFX: &str = "soundeffects/hit/punch_2.wav";
const GROUND_IMPACT_SFX: &str = "soundeffects/hit/punch.wav";
const MAX_COMBAT_SFX_PER_FRAME: usize = 8;
const THROWN_ITEM_HIT_START_SECS: f32 = 0.22;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CombatSfxKind {
    LightHit,
    HeavyHit,
    UltimateHit,
    ItemHit,
    Guarded,
    PigUltimateMunch,
    ThrownItemHit,
    PigJumpX,
    ItemDrop,
    ResultWin,
    ResultLose,
    SteamerExplosion,
    MushroomBigger,
    WallImpact,
    GroundImpact,
}

impl CombatSfxKind {
    const COUNT: usize = 15;

    const fn index(self) -> usize {
        match self {
            Self::LightHit => 0,
            Self::HeavyHit => 1,
            Self::UltimateHit => 2,
            Self::ItemHit => 3,
            Self::Guarded => 4,
            Self::PigUltimateMunch => 5,
            Self::ThrownItemHit => 6,
            Self::PigJumpX => 7,
            Self::ItemDrop => 8,
            Self::ResultWin => 9,
            Self::ResultLose => 10,
            Self::SteamerExplosion => 11,
            Self::MushroomBigger => 12,
            Self::WallImpact => 13,
            Self::GroundImpact => 14,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CombatSfxCue {
    pub kind: CombatSfxKind,
    pub position: Vec3,
    pub priority: u8,
}

impl CombatSfxCue {
    pub fn new(kind: CombatSfxKind, position: Vec3, priority: u8) -> Self {
        Self {
            kind,
            position,
            priority,
        }
    }
}

#[derive(Resource)]
pub struct CombatSfxAssets {
    light_hit: Handle<AudioSource>,
    heavy_hit: Handle<AudioSource>,
    ultimate_hit: Handle<AudioSource>,
    item_hit: Handle<AudioSource>,
    guarded: Handle<AudioSource>,
    pig_ultimate_munch: Handle<AudioSource>,
    thrown_item_hit: Handle<AudioSource>,
    pig_jump_x: Handle<AudioSource>,
    item_drop: Handle<AudioSource>,
    result_win: Handle<AudioSource>,
    result_lose: Handle<AudioSource>,
    steamer_explosion: Handle<AudioSource>,
    mushroom_bigger: Handle<AudioSource>,
    wall_impact: Handle<AudioSource>,
    ground_impact: Handle<AudioSource>,
}

impl CombatSfxAssets {
    fn handle_for(&self, kind: CombatSfxKind) -> Handle<AudioSource> {
        match kind {
            CombatSfxKind::LightHit => self.light_hit.clone(),
            CombatSfxKind::HeavyHit => self.heavy_hit.clone(),
            CombatSfxKind::UltimateHit => self.ultimate_hit.clone(),
            CombatSfxKind::ItemHit => self.item_hit.clone(),
            CombatSfxKind::Guarded => self.guarded.clone(),
            CombatSfxKind::PigUltimateMunch => self.pig_ultimate_munch.clone(),
            CombatSfxKind::ThrownItemHit => self.thrown_item_hit.clone(),
            CombatSfxKind::PigJumpX => self.pig_jump_x.clone(),
            CombatSfxKind::ItemDrop => self.item_drop.clone(),
            CombatSfxKind::ResultWin => self.result_win.clone(),
            CombatSfxKind::ResultLose => self.result_lose.clone(),
            CombatSfxKind::SteamerExplosion => self.steamer_explosion.clone(),
            CombatSfxKind::MushroomBigger => self.mushroom_bigger.clone(),
            CombatSfxKind::WallImpact => self.wall_impact.clone(),
            CombatSfxKind::GroundImpact => self.ground_impact.clone(),
        }
    }
}

pub fn setup_combat_sfx_assets(mut commands: Commands, asset_server: Res<AssetServer>) {
    commands.insert_resource(CombatSfxAssets {
        light_hit: asset_server.load::<AudioSource>(LIGHT_HIT_SFX),
        heavy_hit: asset_server.load::<AudioSource>(HEAVY_HIT_SFX),
        ultimate_hit: asset_server.load::<AudioSource>(ULTIMATE_HIT_SFX),
        item_hit: asset_server.load::<AudioSource>(ITEM_HIT_SFX),
        guarded: asset_server.load::<AudioSource>(GUARDED_SFX),
        pig_ultimate_munch: asset_server.load::<AudioSource>(PIG_ULTIMATE_MUNCH_SFX),
        thrown_item_hit: asset_server.load::<AudioSource>(THROWN_ITEM_HIT_SFX),
        pig_jump_x: asset_server.load::<AudioSource>(PIG_JUMP_X_SFX),
        item_drop: asset_server.load::<AudioSource>(ITEM_DROP_SFX),
        result_win: asset_server.load::<AudioSource>(RESULT_WIN_SFX),
        result_lose: asset_server.load::<AudioSource>(RESULT_LOSE_SFX),
        steamer_explosion: asset_server.load::<AudioSource>(STEAMER_EXPLOSION_SFX),
        mushroom_bigger: asset_server.load::<AudioSource>(MUSHROOM_BIGGER_SFX),
        wall_impact: asset_server.load::<AudioSource>(WALL_IMPACT_SFX),
        ground_impact: asset_server.load::<AudioSource>(GROUND_IMPACT_SFX),
    });
}

pub fn play_combat_sfx(
    mut commands: Commands,
    assets: Res<CombatSfxAssets>,
    mut effects: ResMut<HitEffects>,
) {
    let cues = effects.drain_combat_sfx_cues();
    if cues.is_empty() {
        return;
    }

    for cue in coalesce_combat_sfx_cues(&cues) {
        commands.spawn((
            AudioPlayer::new(assets.handle_for(cue.kind)),
            playback_settings_for_cue(cue),
        ));
    }
}

pub fn combat_sfx_kind_for_impact(profile: &ImpactProfile) -> CombatSfxKind {
    if profile.payload_id == Some(AttackPayloadId::PigUltimateCatch) {
        return CombatSfxKind::PigUltimateMunch;
    }
    if profile.payload_id == Some(AttackPayloadId::PigAirMeatSlam) {
        return CombatSfxKind::PigJumpX;
    }
    if profile.payload_id.is_some_and(payload_is_ultimate_payload) {
        return CombatSfxKind::UltimateHit;
    }
    if matches!(profile.source, ImpactSource::ItemThrow) {
        return CombatSfxKind::ThrownItemHit;
    }
    if matches!(
        profile.source,
        ImpactSource::ItemMelee | ImpactSource::ItemBlast
    ) {
        return CombatSfxKind::ItemHit;
    }
    if profile.feedback.heavy_spark
        || profile.feedback.priority
            >= impact_feedback_profile(ImpactSource::FighterStrike, ImpactFeedbackIntensity::Heavy)
                .priority
    {
        CombatSfxKind::HeavyHit
    } else {
        CombatSfxKind::LightHit
    }
}

pub fn ground_impact_priority(family: ReactionFamilyId) -> u8 {
    match family {
        ReactionFamilyId::AirFishKnockdown => 54,
        ReactionFamilyId::GroundBounceDown => 52,
        ReactionFamilyId::AerialSpikeDown => 50,
        ReactionFamilyId::GroundedDownGetup => 46,
        _ => 42,
    }
}

fn payload_is_ultimate_payload(payload_id: AttackPayloadId) -> bool {
    payload_is_ultimate_catch(payload_id)
        || payload_is_ultimate_scratch(payload_id)
        || payload_is_ultimate_bomb(payload_id)
}

fn coalesce_combat_sfx_cues(cues: &[CombatSfxCue]) -> Vec<CombatSfxCue> {
    let mut strongest_by_kind = [None; CombatSfxKind::COUNT];
    for cue in cues {
        let index = cue.kind.index();
        if strongest_by_kind[index].map_or(true, |selected: CombatSfxCue| {
            cue.priority >= selected.priority
        }) {
            strongest_by_kind[index] = Some(*cue);
        }
    }

    let mut coalesced: Vec<_> = strongest_by_kind.into_iter().flatten().collect();
    coalesced.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then_with(|| a.kind.index().cmp(&b.kind.index()))
    });
    coalesced.truncate(MAX_COMBAT_SFX_PER_FRAME);
    coalesced
}

fn playback_settings_for_cue(cue: CombatSfxCue) -> PlaybackSettings {
    let settings = PlaybackSettings::DESPAWN
        .with_volume(Volume::Linear(volume_for_cue(cue)))
        .with_speed(speed_for_cue(cue));
    if let Some(start_position) = start_position_for_cue(cue.kind) {
        settings.with_start_position(start_position)
    } else {
        settings
    }
}

fn volume_for_cue(cue: CombatSfxCue) -> f32 {
    let base = match cue.kind {
        CombatSfxKind::LightHit => 0.64,
        CombatSfxKind::HeavyHit => 0.86,
        CombatSfxKind::UltimateHit => 1.0,
        CombatSfxKind::ItemHit => 0.82,
        CombatSfxKind::Guarded => 0.9,
        CombatSfxKind::PigUltimateMunch => 0.96,
        CombatSfxKind::ThrownItemHit => 0.88,
        CombatSfxKind::PigJumpX => 0.95,
        CombatSfxKind::ItemDrop => 0.74,
        CombatSfxKind::ResultWin => 0.94,
        CombatSfxKind::ResultLose => 0.94,
        CombatSfxKind::SteamerExplosion => 1.05,
        CombatSfxKind::MushroomBigger => 0.88,
        CombatSfxKind::WallImpact => 0.75,
        CombatSfxKind::GroundImpact => 0.78,
    };
    let priority_scale = (0.78 + cue.priority as f32 / 140.0).clamp(0.78, 1.14);
    (base * priority_scale).clamp(0.0, 1.1)
}

fn speed_for_cue(cue: CombatSfxCue) -> f32 {
    if matches!(
        cue.kind,
        CombatSfxKind::Guarded
            | CombatSfxKind::PigUltimateMunch
            | CombatSfxKind::ThrownItemHit
            | CombatSfxKind::PigJumpX
            | CombatSfxKind::ResultWin
            | CombatSfxKind::ResultLose
            | CombatSfxKind::SteamerExplosion
            | CombatSfxKind::MushroomBigger
    ) {
        return 1.0;
    }

    let seed = cue.position.x * 12.9898
        + cue.position.y * 78.233
        + cue.position.z * 37.719
        + cue.priority as f32 * 0.117;
    1.0 + seed.sin() * 0.035
}

fn start_position_for_cue(kind: CombatSfxKind) -> Option<core::time::Duration> {
    match kind {
        CombatSfxKind::ThrownItemHit => Some(core::time::Duration::from_secs_f32(
            THROWN_ITEM_HIT_START_SECS,
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::{ImpactFeedbackIntensity, ImpactSource, impact_profile};

    fn profile(source: ImpactSource, intensity: ImpactFeedbackIntensity) -> ImpactProfile {
        impact_profile(
            0,
            source,
            10.0,
            6.0,
            2.0,
            false,
            true,
            12.0,
            intensity,
            ReactionFamilyId::ShortStandingStagger,
        )
    }

    #[test]
    fn classifies_light_heavy_ultimate_and_item_impacts() {
        let light = profile(ImpactSource::FighterStrike, ImpactFeedbackIntensity::Light);
        let heavy = profile(ImpactSource::FighterStrike, ImpactFeedbackIntensity::Heavy);
        let mut ultimate = heavy;
        ultimate.payload_id = Some(AttackPayloadId::UltimateBomb);
        let mut pig_ultimate_catch = heavy;
        pig_ultimate_catch.payload_id = Some(AttackPayloadId::PigUltimateCatch);
        let item_melee = profile(ImpactSource::ItemMelee, ImpactFeedbackIntensity::Light);
        let item_blast = profile(ImpactSource::ItemBlast, ImpactFeedbackIntensity::Heavy);
        let item_throw = profile(ImpactSource::ItemThrow, ImpactFeedbackIntensity::Light);

        assert_eq!(combat_sfx_kind_for_impact(&light), CombatSfxKind::LightHit);
        assert_eq!(combat_sfx_kind_for_impact(&heavy), CombatSfxKind::HeavyHit);
        assert_eq!(
            combat_sfx_kind_for_impact(&ultimate),
            CombatSfxKind::UltimateHit
        );
        assert_eq!(
            combat_sfx_kind_for_impact(&pig_ultimate_catch),
            CombatSfxKind::PigUltimateMunch
        );
        assert_eq!(
            combat_sfx_kind_for_impact(&item_melee),
            CombatSfxKind::ItemHit
        );
        assert_eq!(
            combat_sfx_kind_for_impact(&item_blast),
            CombatSfxKind::ItemHit
        );
        assert_eq!(
            combat_sfx_kind_for_impact(&item_throw),
            CombatSfxKind::ThrownItemHit
        );
    }

    #[test]
    fn pig_ultimate_followup_hits_keep_generic_ultimate_sfx() {
        let mut scratch = profile(ImpactSource::FighterStrike, ImpactFeedbackIntensity::Heavy);
        scratch.payload_id = Some(AttackPayloadId::PigUltimateScratchHeavy);
        let mut bomb = scratch;
        bomb.payload_id = Some(AttackPayloadId::PigUltimateBomb);

        assert_eq!(
            combat_sfx_kind_for_impact(&scratch),
            CombatSfxKind::UltimateHit
        );
        assert_eq!(
            combat_sfx_kind_for_impact(&bomb),
            CombatSfxKind::UltimateHit
        );
    }

    #[test]
    fn pig_jump_x_uses_dedicated_hit_sfx() {
        let mut pig_jump_x = profile(ImpactSource::FighterStrike, ImpactFeedbackIntensity::Heavy);
        pig_jump_x.payload_id = Some(AttackPayloadId::PigAirMeatSlam);
        let mut pig_jump_belly =
            profile(ImpactSource::FighterStrike, ImpactFeedbackIntensity::Heavy);
        pig_jump_belly.payload_id = Some(AttackPayloadId::PigJumpBellyDrop);

        assert_eq!(
            combat_sfx_kind_for_impact(&pig_jump_x),
            CombatSfxKind::PigJumpX
        );
        assert_eq!(
            combat_sfx_kind_for_impact(&pig_jump_belly),
            CombatSfxKind::HeavyHit
        );
    }

    #[test]
    fn projectile_impacts_fall_back_to_feedback_weight() {
        let light = profile(ImpactSource::Projectile, ImpactFeedbackIntensity::Light);
        let heavy = profile(ImpactSource::Projectile, ImpactFeedbackIntensity::Heavy);

        assert_eq!(combat_sfx_kind_for_impact(&light), CombatSfxKind::LightHit);
        assert_eq!(combat_sfx_kind_for_impact(&heavy), CombatSfxKind::HeavyHit);
    }

    #[test]
    fn hit_effects_sound_cues_are_drained_once() {
        let mut effects = HitEffects::default();
        effects.push_combat_sfx(CombatSfxCue::new(CombatSfxKind::LightHit, Vec3::ZERO, 20));

        assert_eq!(effects.drain_combat_sfx_cues().len(), 1);
        assert!(effects.drain_combat_sfx_cues().is_empty());
    }

    #[test]
    fn authored_timing_sensitive_sfx_keep_fixed_playback_timing() {
        let item_hit = CombatSfxCue::new(CombatSfxKind::ThrownItemHit, Vec3::ZERO, 50);
        let pig_jump_x = CombatSfxCue::new(CombatSfxKind::PigJumpX, Vec3::X, 70);
        let guarded = CombatSfxCue::new(CombatSfxKind::Guarded, Vec3::Y, 70);

        assert_eq!(
            start_position_for_cue(CombatSfxKind::ThrownItemHit),
            Some(core::time::Duration::from_secs_f32(
                THROWN_ITEM_HIT_START_SECS
            ))
        );
        assert_eq!(start_position_for_cue(CombatSfxKind::PigJumpX), None);
        assert_eq!(start_position_for_cue(CombatSfxKind::Guarded), None);
        assert_eq!(
            playback_settings_for_cue(item_hit).start_position,
            Some(core::time::Duration::from_secs_f32(
                THROWN_ITEM_HIT_START_SECS
            ))
        );
        assert_eq!(speed_for_cue(item_hit), 1.0);
        assert_eq!(speed_for_cue(pig_jump_x), 1.0);
        assert_eq!(speed_for_cue(guarded), 1.0);
        assert!(volume_for_cue(guarded) > volume_for_cue(item_hit));
    }

    #[test]
    fn coalescing_keeps_the_strongest_cue_per_kind_and_caps_total() {
        let cues = vec![
            CombatSfxCue::new(CombatSfxKind::LightHit, Vec3::ZERO, 10),
            CombatSfxCue::new(CombatSfxKind::LightHit, Vec3::X, 90),
            CombatSfxCue::new(CombatSfxKind::HeavyHit, Vec3::Y, 40),
            CombatSfxCue::new(CombatSfxKind::UltimateHit, Vec3::Z, 80),
            CombatSfxCue::new(CombatSfxKind::ItemHit, Vec3::ONE, 35),
            CombatSfxCue::new(CombatSfxKind::PigUltimateMunch, Vec3::ONE, 82),
            CombatSfxCue::new(CombatSfxKind::ThrownItemHit, Vec3::ONE, 50),
            CombatSfxCue::new(CombatSfxKind::ItemDrop, Vec3::ONE, 45),
            CombatSfxCue::new(CombatSfxKind::ResultWin, Vec3::ONE, 120),
            CombatSfxCue::new(CombatSfxKind::SteamerExplosion, Vec3::ONE, 92),
            CombatSfxCue::new(CombatSfxKind::MushroomBigger, Vec3::ONE, 65),
            CombatSfxCue::new(CombatSfxKind::WallImpact, Vec3::NEG_X, 25),
            CombatSfxCue::new(CombatSfxKind::GroundImpact, Vec3::NEG_Y, 20),
        ];

        let coalesced = coalesce_combat_sfx_cues(&cues);

        assert_eq!(coalesced.len(), MAX_COMBAT_SFX_PER_FRAME);
        assert!(coalesced.contains(&CombatSfxCue::new(CombatSfxKind::LightHit, Vec3::X, 90)));
        assert!(!coalesced.contains(&CombatSfxCue::new(CombatSfxKind::LightHit, Vec3::ZERO, 10)));
        assert_eq!(coalesced[0].kind, CombatSfxKind::ResultWin);
    }
}
