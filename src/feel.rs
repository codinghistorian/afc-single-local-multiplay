use std::{
    path::{Path, PathBuf},
    time::SystemTime,
};

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use std::fs;

use bevy::prelude::*;
use serde::Deserialize;

use crate::{
    characters::CharacterKind,
    components::FighterAction,
    effects::HitImpactEffectId,
    reactions::{QueuedAftermath, ReactionFamilyDef, ReactionFamilyId},
    techniques::{
        AttackPayloadDef, AttackPayloadId, FeedbackPhase, MoveTimelineEvent, MoveTimelineEventKind,
        MsTimingWindow, TechniqueDefinition, TechniqueId,
    },
};

const COMBAT_FEEL_PATH: &str = "assets/feel/combat_overrides.ron";

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default)]
pub struct CombatFeelFile {
    pub hit_effects_enabled: bool,
    pub reactions: Vec<ReactionOverride>,
    pub payloads: Vec<PayloadOverride>,
    pub hit_effects: Vec<HitEffectOverride>,
    pub techniques: Vec<TechniqueOverride>,
    pub poses: Vec<ActionPoseOverride>,
}

impl Default for CombatFeelFile {
    fn default() -> Self {
        Self {
            hit_effects_enabled: true,
            reactions: Vec::new(),
            payloads: Vec::new(),
            hit_effects: Vec::new(),
            techniques: Vec::new(),
            poses: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default)]
pub struct ReactionOverride {
    pub id: ReactionFamilyId,
    pub horizontal_scale: Option<f32>,
    pub vertical_scale: Option<f32>,
    pub hitstun_recover_ms: Option<u32>,
    pub grounded_getup_ms: Option<u32>,
    pub grounded_recover_ms: Option<u32>,
    pub grounded_stick_ms: Option<u32>,
    pub landing_getup_transition_ms: Option<u32>,
    pub landing_recover_ms: Option<u32>,
    pub landing_stick_ms: Option<u32>,
    pub landing_horizontal_damping: Option<f32>,
    pub priority_bonus: Option<u8>,
}

impl Default for ReactionOverride {
    fn default() -> Self {
        Self {
            id: ReactionFamilyId::ShortStandingStagger,
            horizontal_scale: None,
            vertical_scale: None,
            hitstun_recover_ms: None,
            grounded_getup_ms: None,
            grounded_recover_ms: None,
            grounded_stick_ms: None,
            landing_getup_transition_ms: None,
            landing_recover_ms: None,
            landing_stick_ms: None,
            landing_horizontal_damping: None,
            priority_bonus: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default)]
pub struct PayloadOverride {
    pub id: AttackPayloadId,
    pub power: Option<f32>,
    pub str_scale: Option<f32>,
    pub time_ms: Option<u32>,
    pub damage: Option<f32>,
    pub knockback: Option<f32>,
    pub vertical_knockback: Option<f32>,
    pub hitstop_scale: Option<f32>,
    pub shake_scale: Option<f32>,
    pub feedback_priority_bonus: Option<u8>,
}

impl Default for PayloadOverride {
    fn default() -> Self {
        Self {
            id: AttackPayloadId::AsBeat1,
            power: None,
            str_scale: None,
            time_ms: None,
            damage: None,
            knockback: None,
            vertical_knockback: None,
            hitstop_scale: None,
            shake_scale: None,
            feedback_priority_bonus: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default)]
pub struct HitEffectOverride {
    pub character: CharacterKind,
    pub technique: TechniqueId,
    pub payload: AttackPayloadId,
    pub effect: HitImpactEffectId,
}

impl Default for HitEffectOverride {
    fn default() -> Self {
        Self {
            character: CharacterKind::Cat,
            technique: TechniqueId::CatLight1,
            payload: AttackPayloadId::AsBeat1,
            effect: HitImpactEffectId::GenericLight,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default)]
pub struct TechniqueOverride {
    pub id: TechniqueId,
    pub animation_recovery_ms: Option<u32>,
    pub next_tech_ms: Option<u32>,
    pub recover_ms: Option<u32>,
    pub input_buffer_ms: Option<u32>,
    pub cancel_start_ms: Option<u32>,
    pub cancel_end_ms: Option<u32>,
    pub branch_start_ms: Option<u32>,
    pub branch_end_ms: Option<u32>,
    pub events: Vec<TimelineEventOverride>,
}

impl Default for TechniqueOverride {
    fn default() -> Self {
        Self {
            id: TechniqueId::CatLight1,
            animation_recovery_ms: None,
            next_tech_ms: None,
            recover_ms: None,
            input_buffer_ms: None,
            cancel_start_ms: None,
            cancel_end_ms: None,
            branch_start_ms: None,
            branch_end_ms: None,
            events: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default)]
pub struct TimelineEventOverride {
    pub key: TimelineEventKey,
    pub at_ms: Option<u32>,
    pub forward: Option<f32>,
    pub lift: Option<f32>,
}

impl Default for TimelineEventOverride {
    fn default() -> Self {
        Self {
            key: TimelineEventKey::NextTech,
            at_ms: None,
            forward: None,
            lift: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub enum TimelineEventKey {
    Attack(AttackPayloadId),
    Feedback(FeedbackPhase),
    Motion(usize),
    NextTech,
    Recover,
    Stop,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default)]
pub struct ActionPoseOverride {
    pub action: FighterAction,
    pub pitch: Option<f32>,
    pub yaw: Option<f32>,
    pub roll: Option<f32>,
    pub scale: Option<[f32; 3]>,
}

impl Default for ActionPoseOverride {
    fn default() -> Self {
        Self {
            action: FighterAction::Idle,
            pitch: None,
            yaw: None,
            roll: None,
            scale: None,
        }
    }
}

#[derive(Resource, Clone, Debug)]
pub struct CombatFeelTuning {
    overrides: CombatFeelFile,
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    path: PathBuf,
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    last_modified: Option<SystemTime>,
    last_error: Option<String>,
}

impl Default for CombatFeelTuning {
    fn default() -> Self {
        Self::load_initial(COMBAT_FEEL_PATH)
    }
}

impl CombatFeelTuning {
    #[cfg(test)]
    pub fn from_overrides(overrides: CombatFeelFile) -> Self {
        Self {
            overrides,
            path: PathBuf::from(COMBAT_FEEL_PATH),
            last_modified: None,
            last_error: None,
        }
    }

    fn load_initial(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        match load_combat_feel_file(&path) {
            Ok((overrides, modified)) => Self {
                overrides,
                path,
                last_modified: modified,
                last_error: None,
            },
            Err(error) => Self {
                overrides: CombatFeelFile::default(),
                path,
                last_modified: None,
                last_error: Some(error),
            },
        }
    }

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    fn reload_if_changed(&mut self) -> bool {
        let modified = match fs::metadata(&self.path).and_then(|metadata| metadata.modified()) {
            Ok(modified) => Some(modified),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if self.last_modified.is_some() {
                    self.overrides = CombatFeelFile::default();
                    self.last_modified = None;
                    self.last_error = None;
                    return true;
                }
                return false;
            }
            Err(error) => {
                self.last_error = Some(error.to_string());
                return false;
            }
        };

        if modified == self.last_modified {
            return false;
        }

        match load_combat_feel_file(&self.path) {
            Ok((overrides, modified)) => {
                self.overrides = overrides;
                self.last_modified = modified;
                self.last_error = None;
                true
            }
            Err(error) => {
                self.last_modified = modified;
                self.last_error = Some(error);
                false
            }
        }
    }

    pub fn apply_reaction(&self, mut reaction: ReactionFamilyDef) -> ReactionFamilyDef {
        let Some(override_def) = self.reaction_override(reaction.id) else {
            return reaction;
        };

        if let Some(value) = override_def.horizontal_scale {
            reaction.horizontal_scale = value;
        }
        if let Some(value) = override_def.vertical_scale {
            reaction.vertical_scale = value;
        }
        if let Some(value) = override_def.hitstun_recover_ms {
            reaction.hitstun_recover_ms = Some(value);
        }
        if let Some(value) = override_def.grounded_getup_ms {
            reaction.grounded_getup_ms = Some(value);
        }
        if let Some(value) = override_def.grounded_recover_ms {
            reaction.grounded_recover_ms = Some(value);
        }
        if let Some(value) = override_def.grounded_stick_ms {
            reaction.grounded_stick_ms = value;
        }
        if let Some(value) = override_def.priority_bonus {
            reaction.priority_bonus = value;
        }
        if let Some(mut aftermath) = reaction.landing_aftermath {
            apply_landing_aftermath_override(&mut aftermath, override_def);
            reaction.landing_aftermath = Some(aftermath);
        }
        reaction
    }

    pub fn apply_payload(&self, mut payload: AttackPayloadDef) -> AttackPayloadDef {
        let Some(override_def) = self.payload_override(payload.id) else {
            return payload;
        };

        if let Some(value) = override_def.power {
            payload.power = value;
        }
        if let Some(value) = override_def.str_scale {
            payload.str_scale = value;
        }
        if let Some(value) = override_def.time_ms {
            payload.time_ms = value;
        }
        if let Some(value) = override_def.damage {
            payload.damage = value;
        }
        if let Some(value) = override_def.knockback {
            payload.knockback = value;
        }
        if let Some(value) = override_def.vertical_knockback {
            payload.vertical_knockback = value;
        }
        if let Some(value) = override_def.hitstop_scale {
            payload.hitstop_scale = value;
        }
        if let Some(value) = override_def.shake_scale {
            payload.shake_scale = value;
        }
        if let Some(value) = override_def.feedback_priority_bonus {
            payload.feedback_priority_bonus = value;
        }
        payload
    }

    pub fn apply_technique(&self, mut technique: TechniqueDefinition) -> TechniqueDefinition {
        let Some(override_def) = self.technique_override(technique.id) else {
            return technique;
        };

        if let Some(value) = override_def.animation_recovery_ms {
            technique.script.animation_recovery_ms = Some(value);
        }
        if let Some(value) = override_def.next_tech_ms {
            technique.script.next_tech_ms = Some(value);
        }
        if let Some(value) = override_def.recover_ms {
            technique.script.recover_ms = value;
        }
        if let Some(value) = override_def.input_buffer_ms {
            technique.input_buffer_ms = value;
        }
        if let Some(window) =
            timing_window(override_def.cancel_start_ms, override_def.cancel_end_ms)
        {
            technique.cancel_window = Some(window);
        }
        if let Some(window) =
            timing_window(override_def.branch_start_ms, override_def.branch_end_ms)
        {
            technique.branch_window = Some(window);
        }
        technique
    }

    pub fn timeline_event_at_ms(
        &self,
        technique: &TechniqueDefinition,
        event_index: usize,
        event: &MoveTimelineEvent,
    ) -> u32 {
        self.timeline_event_override(technique, event_index, event)
            .and_then(|override_def| override_def.at_ms)
            .unwrap_or(event.at_ms)
    }

    pub fn timeline_motion(
        &self,
        technique: &TechniqueDefinition,
        event_index: usize,
        event: &MoveTimelineEvent,
        forward: f32,
        lift: f32,
    ) -> (f32, f32) {
        let Some(override_def) = self.timeline_event_override(technique, event_index, event) else {
            return (forward, lift);
        };

        (
            override_def.forward.unwrap_or(forward),
            override_def.lift.unwrap_or(lift),
        )
    }

    pub fn pose_override(&self, action: FighterAction) -> Option<&ActionPoseOverride> {
        self.overrides
            .poses
            .iter()
            .rev()
            .find(|override_def| override_def.action == action)
    }

    pub fn hit_effect_for_payload(
        &self,
        character: CharacterKind,
        technique: TechniqueId,
        payload: AttackPayloadId,
    ) -> Option<HitImpactEffectId> {
        self.hit_effect_override(character, technique, payload)
            .map(|override_def| override_def.effect)
    }

    pub fn hit_effects_enabled(&self) -> bool {
        self.overrides.hit_effects_enabled
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    fn reaction_override(&self, id: ReactionFamilyId) -> Option<&ReactionOverride> {
        self.overrides
            .reactions
            .iter()
            .rev()
            .find(|override_def| override_def.id == id)
    }

    fn payload_override(&self, id: AttackPayloadId) -> Option<&PayloadOverride> {
        self.overrides
            .payloads
            .iter()
            .rev()
            .find(|override_def| override_def.id == id)
    }

    fn hit_effect_override(
        &self,
        character: CharacterKind,
        technique: TechniqueId,
        payload: AttackPayloadId,
    ) -> Option<&HitEffectOverride> {
        self.overrides
            .hit_effects
            .iter()
            .rev()
            .find(|override_def| {
                override_def.character == character
                    && override_def.technique == technique
                    && override_def.payload == payload
            })
    }

    fn technique_override(&self, id: TechniqueId) -> Option<&TechniqueOverride> {
        self.technique_override_exact(id)
    }

    fn technique_override_exact(&self, id: TechniqueId) -> Option<&TechniqueOverride> {
        self.overrides
            .techniques
            .iter()
            .rev()
            .find(|override_def| override_def.id == id)
    }

    fn timeline_event_override(
        &self,
        technique: &TechniqueDefinition,
        event_index: usize,
        event: &MoveTimelineEvent,
    ) -> Option<&TimelineEventOverride> {
        let technique_override = self.technique_override(technique.id)?;
        let key = timeline_event_key(technique, event_index, event)?;
        technique_override
            .events
            .iter()
            .rev()
            .find(|override_def| override_def.key == key)
    }
}

pub fn setup_combat_feel_tuning(mut commands: Commands) {
    let tuning = CombatFeelTuning::default();
    if let Some(error) = tuning.last_error() {
        warn!("Combat feel tuning started with defaults: {error}");
    }
    commands.insert_resource(tuning);
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub fn reload_combat_feel_tuning(mut tuning: ResMut<CombatFeelTuning>) {
    let previous_error = tuning.last_error().map(str::to_owned);
    if tuning.reload_if_changed() {
        info!("Reloaded combat feel tuning from {}", tuning.path.display());
    } else if tuning.last_error().map(str::to_owned) != previous_error
        && let Some(error) = tuning.last_error()
    {
        warn!("Keeping last valid combat feel tuning: {error}");
    }
}

fn load_combat_feel_file(path: &Path) -> Result<(CombatFeelFile, Option<SystemTime>), String> {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = path;
        let contents = include_str!("../assets/feel/combat_overrides.ron");
        let overrides: CombatFeelFile =
            ron::from_str(contents).map_err(|error| format!("RON parse failed: {error}"))?;
        return Ok((overrides, None));
    }

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    {
        if !path.exists() {
            return Ok((CombatFeelFile::default(), None));
        }

        let contents = fs::read_to_string(path).map_err(|error| error.to_string())?;
        let overrides: CombatFeelFile =
            ron::from_str(&contents).map_err(|error| format!("RON parse failed: {error}"))?;
        let modified = fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .map_err(|error| error.to_string())?;
        Ok((overrides, Some(modified)))
    }
}

fn apply_landing_aftermath_override(
    aftermath: &mut QueuedAftermath,
    override_def: &ReactionOverride,
) {
    if let Some(value) = override_def.landing_getup_transition_ms {
        aftermath.getup_transition_ms = value;
    }
    if let Some(value) = override_def.landing_recover_ms {
        aftermath.recover_ms = value;
    }
    if let Some(value) = override_def.landing_stick_ms {
        aftermath.landing_stick_ms = value;
    }
    if let Some(value) = override_def.landing_horizontal_damping {
        aftermath.horizontal_damping = value;
    }
}

fn timing_window(start_ms: Option<u32>, end_ms: Option<u32>) -> Option<MsTimingWindow> {
    let start_ms = start_ms?;
    Some(MsTimingWindow { start_ms, end_ms })
}

fn timeline_event_key(
    technique: &TechniqueDefinition,
    event_index: usize,
    event: &MoveTimelineEvent,
) -> Option<TimelineEventKey> {
    Some(match event.kind {
        MoveTimelineEventKind::Attack(payload) => TimelineEventKey::Attack(payload),
        MoveTimelineEventKind::ChargedAttack { full, .. } => TimelineEventKey::Attack(full),
        MoveTimelineEventKind::SpawnBeeSkill(_) | MoveTimelineEventKind::SpawnPenguinSkill(_) => {
            return None;
        }
        MoveTimelineEventKind::Feedback(phase, _) => TimelineEventKey::Feedback(phase),
        MoveTimelineEventKind::Motion { .. } => {
            TimelineEventKey::Motion(motion_event_index(technique, event_index)?)
        }
        MoveTimelineEventKind::NextTech => TimelineEventKey::NextTech,
        MoveTimelineEventKind::Recover => TimelineEventKey::Recover,
        MoveTimelineEventKind::Stop => TimelineEventKey::Stop,
    })
}

fn motion_event_index(technique: &TechniqueDefinition, event_index: usize) -> Option<usize> {
    technique
        .script
        .events
        .iter()
        .take(event_index + 1)
        .filter(|event| matches!(event.kind, MoveTimelineEventKind::Motion { .. }))
        .count()
        .checked_sub(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        reactions::reaction_family_definition,
        techniques::{
            AttackPayloadId, MsTimingWindow, TechniqueId, attack_payload_definition,
            technique_definition_by_id,
        },
    };

    #[test]
    fn committed_combat_feel_file_parses() {
        let path = Path::new(COMBAT_FEEL_PATH);
        assert!(path.exists());
        let (file, _) = load_combat_feel_file(path).unwrap();
        assert!(!file.hit_effects_enabled);
        assert!(!file.reactions.is_empty());
        assert!(!file.payloads.is_empty());
        assert!(!file.hit_effects.is_empty());
        assert!(!file.techniques.is_empty());
    }

    #[test]
    fn hit_effect_toggle_defaults_on() {
        let tuning = CombatFeelTuning::from_overrides(CombatFeelFile::default());
        assert!(tuning.hit_effects_enabled());
    }

    #[test]
    fn committed_hit_effects_split_cat_and_pig_light_impacts() {
        let path = Path::new(COMBAT_FEEL_PATH);
        let (file, _) = load_combat_feel_file(path).unwrap();
        let tuning = CombatFeelTuning::from_overrides(file);

        assert_eq!(
            tuning.hit_effect_for_payload(
                CharacterKind::Cat,
                TechniqueId::CatLight1,
                AttackPayloadId::AsBeat1
            ),
            Some(HitImpactEffectId::GenericLight)
        );
        assert_eq!(
            tuning.hit_effect_for_payload(
                CharacterKind::Pig,
                TechniqueId::PigLight1,
                AttackPayloadId::AsBeat1
            ),
            Some(HitImpactEffectId::LightBlue)
        );
    }

    #[test]
    fn reaction_override_can_speed_up_short_stagger() {
        let tuning = CombatFeelTuning::from_overrides(CombatFeelFile {
            reactions: vec![ReactionOverride {
                id: ReactionFamilyId::ShortStandingStagger,
                hitstun_recover_ms: Some(280),
                ..default()
            }],
            ..default()
        });

        let reaction = tuning.apply_reaction(reaction_family_definition(
            ReactionFamilyId::ShortStandingStagger,
        ));
        assert_eq!(reaction.hitstun_recover_ms, Some(280));
        assert_eq!(
            reaction_family_definition(ReactionFamilyId::ShortStandingStagger).hitstun_recover_ms,
            Some(500)
        );
    }

    #[test]
    fn payload_override_preserves_unspecified_authored_fields() {
        let tuning = CombatFeelTuning::from_overrides(CombatFeelFile {
            payloads: vec![PayloadOverride {
                id: AttackPayloadId::AsBeat1,
                damage: Some(4.25),
                time_ms: Some(90),
                ..default()
            }],
            ..default()
        });

        let base = attack_payload_definition(AttackPayloadId::AsBeat1);
        let payload = tuning.apply_payload(base);
        assert_eq!(payload.damage, 4.25);
        assert_eq!(payload.time_ms, 90);
        assert_eq!(payload.knockback, base.knockback);
    }

    #[test]
    fn hit_effect_override_distinguishes_shared_character_payloads() {
        let tuning = CombatFeelTuning::from_overrides(CombatFeelFile {
            hit_effects: vec![
                HitEffectOverride {
                    character: CharacterKind::Cat,
                    technique: TechniqueId::CatLight1,
                    payload: AttackPayloadId::AsBeat1,
                    effect: HitImpactEffectId::GenericLight,
                },
                HitEffectOverride {
                    character: CharacterKind::Pig,
                    technique: TechniqueId::PigLight1,
                    payload: AttackPayloadId::AsBeat1,
                    effect: HitImpactEffectId::LightBlue,
                },
            ],
            ..default()
        });

        assert_eq!(
            tuning.hit_effect_for_payload(
                CharacterKind::Cat,
                TechniqueId::CatLight1,
                AttackPayloadId::AsBeat1
            ),
            Some(HitImpactEffectId::GenericLight)
        );
        assert_eq!(
            tuning.hit_effect_for_payload(
                CharacterKind::Pig,
                TechniqueId::PigLight1,
                AttackPayloadId::AsBeat1
            ),
            Some(HitImpactEffectId::LightBlue)
        );
        assert_eq!(
            tuning.hit_effect_for_payload(
                CharacterKind::Pig,
                TechniqueId::PigLight2,
                AttackPayloadId::AsBeat1
            ),
            None
        );
    }

    #[test]
    fn technique_override_changes_recover_and_event_timing() {
        let tuning = CombatFeelTuning::from_overrides(CombatFeelFile {
            techniques: vec![TechniqueOverride {
                id: TechniqueId::CatLight1,
                recover_ms: Some(700),
                events: vec![TimelineEventOverride {
                    key: TimelineEventKey::Attack(AttackPayloadId::AsBeat1),
                    at_ms: Some(390),
                    ..default()
                }],
                ..default()
            }],
            ..default()
        });

        let base = technique_definition_by_id(TechniqueId::CatLight1).unwrap();
        let technique = tuning.apply_technique(base);
        let first_attack = technique
            .script
            .events
            .iter()
            .enumerate()
            .find(|(_, event)| {
                matches!(
                    event.kind,
                    MoveTimelineEventKind::Attack(AttackPayloadId::AsBeat1)
                )
            })
            .unwrap();

        assert_eq!(technique.script.recover_ms, 700);
        assert_eq!(
            tuning.timeline_event_at_ms(&technique, first_attack.0, first_attack.1),
            390
        );
    }

    #[test]
    fn committed_combat_feel_shortens_representative_attack_gaps() {
        let path = Path::new(COMBAT_FEEL_PATH);
        let (file, _) = load_combat_feel_file(path).unwrap();
        let tuning = CombatFeelTuning::from_overrides(file);

        let light =
            tuning.apply_technique(technique_definition_by_id(TechniqueId::CatLight1).unwrap());
        let panda_light =
            tuning.apply_technique(technique_definition_by_id(TechniqueId::PandaLight1).unwrap());
        let dog_heavy =
            tuning.apply_technique(technique_definition_by_id(TechniqueId::DogHeavy).unwrap());
        let pig_combo = tuning
            .apply_technique(technique_definition_by_id(TechniqueId::PigComboFinisher).unwrap());
        let pig_ultimate = tuning
            .apply_technique(technique_definition_by_id(TechniqueId::PigUltimateStartup).unwrap());

        assert_eq!(light.script.recover_ms, 230);
        assert!(panda_light.script.recover_ms < 860);
        assert!(dog_heavy.script.recover_ms < 620);
        assert!(pig_combo.script.recover_ms < 1280);
        assert!(pig_ultimate.script.recover_ms < 2200);
    }

    #[test]
    fn committed_light_fire_punch_feedback_fires_before_short_recovery() {
        let path = Path::new(COMBAT_FEEL_PATH);
        let (file, _) = load_combat_feel_file(path).unwrap();
        let tuning = CombatFeelTuning::from_overrides(file);

        for (id, payload) in [
            (TechniqueId::CatLight1, AttackPayloadId::AsBeat1),
            (TechniqueId::CatLight2, AttackPayloadId::AssBeat1),
            (TechniqueId::PigLight1, AttackPayloadId::AsBeat1),
            (TechniqueId::PigLight2, AttackPayloadId::AssBeat1),
        ] {
            let technique = tuning.apply_technique(technique_definition_by_id(id).unwrap());
            let prehit = technique
                .script
                .events
                .iter()
                .enumerate()
                .find(|(_, event)| {
                    matches!(
                        event.kind,
                        MoveTimelineEventKind::Feedback(FeedbackPhase::PreHit, _)
                    )
                })
                .unwrap();
            let attack = technique
                .script
                .events
                .iter()
                .enumerate()
                .find(|(_, event)| event.kind == MoveTimelineEventKind::Attack(payload))
                .unwrap();
            let prehit_ms = tuning.timeline_event_at_ms(&technique, prehit.0, prehit.1);
            let attack_ms = tuning.timeline_event_at_ms(&technique, attack.0, attack.1);

            assert!(prehit_ms <= attack_ms);
            assert!(prehit_ms < technique.script.animation_recovery_ms.unwrap());
            assert!(prehit_ms < technique.script.recover_ms);
        }
    }

    #[test]
    fn committed_combat_feel_matches_pig_input_timing_to_cat() {
        let path = Path::new(COMBAT_FEEL_PATH);
        let (file, _) = load_combat_feel_file(path).unwrap();
        let tuning = CombatFeelTuning::from_overrides(file);

        let cat_combo = tuning
            .apply_technique(technique_definition_by_id(TechniqueId::CatComboFinisher).unwrap());
        let pig_combo = tuning
            .apply_technique(technique_definition_by_id(TechniqueId::PigComboFinisher).unwrap());
        let cat_heavy =
            tuning.apply_technique(technique_definition_by_id(TechniqueId::CatHeavy).unwrap());
        let pig_heavy =
            tuning.apply_technique(technique_definition_by_id(TechniqueId::PigHeavy).unwrap());
        let cat_heavy2 =
            tuning.apply_technique(technique_definition_by_id(TechniqueId::CatHeavy2).unwrap());
        let pig_heavy2 =
            tuning.apply_technique(technique_definition_by_id(TechniqueId::PigHeavy2).unwrap());

        assert_eq!(pig_combo.script.next_tech_ms, cat_combo.script.next_tech_ms);
        assert_eq!(pig_combo.script.recover_ms, cat_combo.script.recover_ms);
        assert_eq!(pig_combo.input_buffer_ms, cat_combo.input_buffer_ms);
        assert_eq!(pig_combo.branch_window, cat_combo.branch_window);

        assert_eq!(pig_heavy.script.next_tech_ms, cat_heavy.script.next_tech_ms);
        assert_eq!(pig_heavy.script.recover_ms, cat_heavy.script.recover_ms);
        assert_eq!(pig_heavy.input_buffer_ms, cat_heavy.input_buffer_ms);
        assert_eq!(
            pig_heavy.branch_window,
            Some(MsTimingWindow::closed(0, 340))
        );
        assert_eq!(pig_heavy.branch_window, cat_heavy.branch_window);
        assert_eq!(pig_heavy.cancel_window, cat_heavy.cancel_window);

        assert_eq!(
            pig_heavy2.script.next_tech_ms,
            cat_heavy2.script.next_tech_ms
        );
        assert_eq!(pig_heavy2.script.recover_ms, cat_heavy2.script.recover_ms);
        assert_eq!(pig_heavy2.input_buffer_ms, cat_heavy2.input_buffer_ms);
        assert_eq!(pig_heavy2.branch_window, cat_heavy2.branch_window);
    }
}
