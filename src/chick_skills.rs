use bevy::ecs::system::SystemParam;
use bevy::gltf::GltfAssetLabel;
use bevy::prelude::*;
use std::f32::consts::TAU;

use crate::arena::ground_support_for_arena_with_radius;
use crate::arena_defs::{ActiveArena, ArenaDefinition};
use crate::bee_skills::BeeSkillTargetSnapshot;
use crate::canonical_math;
use crate::combat::{
    HitEffects, ImpactSource, can_receive_impact, impact_profile_from_payload_with_feel,
};
use crate::components::{Fighter, FighterActionState, FighterMotor, FighterStats, SimPosition};
use crate::constants::{ARENA_TOP_Y, FIGHTER_HEIGHT, FIGHTER_RADIUS};
use crate::contact_arbitration::{
    ContactBuffer, ContactFlags, ContactOutcomeKind, ContactPhase, ContactRecord, ContactSourceKind,
};
use crate::determinism::{FighterHitMask, FighterId, SimEntityId, SimEntityKind};
use crate::ecs_identity::{SimulationIdentityAllocator, StableSimEntity, despawn_stable};
use crate::effects::{EffectAssets, FeedbackPackageId, spawn_feedback_package};
use crate::feel::CombatFeelTuning;
use crate::game_state::{Hitstop, MatchState};
use crate::rollback::RollbackEventDiscard;
use crate::sim_event::{
    AbilityLifecycleEvent, EventEmitError, MAX_SIM_EVENTS_PER_TICK, SIM_EVENT_HISTORY_TICKS,
    SimEvent, SimEventId, SimEventKind, SimEventSource, TickEventBuffer,
};
use crate::simulation::{ElapsedTicks, SIM_HZ_U32, SimTick, TickTimer};
use crate::styles::FighterStyleKind;
use crate::techniques::{AttackPayloadId, AttackShapeId, ChickSkillId, TechniqueId};

pub const CHICK_EGG_ASSET: &str = "food/kenney_food_kit/egg.glb";
pub const CHICK_EGG_HALF_ASSET: &str = "food/kenney_food_kit/egg-half.glb";
pub const CHICK_EGG_COOKED_ASSET: &str = "food/kenney_food_kit/egg-cooked.glb";
pub const CHICK_EGG_CUP_ASSET: &str = "food/kenney_food_kit/egg-cup.glb";
pub const CHICK_EGGPLANT_ASSET: &str = "food/kenney_food_kit/eggplant.glb";

const CHICK_SKILL_LOCK_RANGE: f32 = 7.2;
const CHICK_SKILL_LOCK_CONE_DOT: f32 = 0.70710677;
const CHICK_SHELL_CHIP_SPEED: f32 = 9.6;
pub const CHICK_SHELL_CHIP_LIFETIME: f32 = 0.62;
pub const CHICK_SHELL_CHIP_RADIUS: f32 = 0.25;
const CHICK_FRIED_DISC_SPEED: f32 = 7.4;
pub const CHICK_FRIED_DISC_LIFETIME: f32 = 0.74;
pub const CHICK_FRIED_DISC_RADIUS: f32 = 0.34;
const CHICK_EGG_CUP_SPEED: f32 = 5.8;
const CHICK_EGG_CUP_LIFT: f32 = 3.95;
const CHICK_EGG_CUP_GRAVITY: f32 = 9.6;
pub const CHICK_EGG_CUP_LIFETIME: f32 = 1.12;
pub const CHICK_EGG_CUP_RADIUS: f32 = 0.42;
pub const CHICK_ORBIT_EGG_LIFETIME: f32 = 8.0;
pub const CHICK_ORBIT_EGG_RADIUS: f32 = 0.36;
const CHICK_ORBIT_EGG_LAUNCH_SPEED: f32 = 18.0;
pub const CHICK_ORBIT_EGG_LAUNCH_LIFETIME: f32 = 1.0;
pub const CHICK_ORBIT_EGG_LAUNCH_RADIUS: f32 = 0.78;
const CHICK_ORBIT_EGG_RETURN_SPEED: f32 = 18.0;
pub const CHICK_ORBIT_EGG_RETURN_LIFETIME: f32 = 1.2;
pub const CHICK_ORBIT_EGG_RETURN_RADIUS: f32 = CHICK_ORBIT_EGG_LAUNCH_RADIUS;
const CHICK_ORBIT_EGG_RETURN_ARRIVAL_DISTANCE: f32 = 0.14;
const CHICK_ORBIT_EGG_ORBIT_RADIUS: f32 = 0.95;
const CHICK_ORBIT_EGG_HEIGHT: f32 = 1.0;
const CHICK_ORBIT_EGG_ANGULAR_SPEED: f32 = TAU * 0.85;
const CHICK_ORBIT_EGG_VISUAL_SCALE: f32 = 5.0;
pub const CHICK_ULTIMATE_EGG_COUNT: usize = 16;
pub const CHICK_ULTIMATE_EGG_LIFETIME: f32 = 4.0;
const CHICK_ULTIMATE_EGG_SPAWN_RADIUS: f32 = 0.72;
const CHICK_FRESH_EGG_FORWARD_SPEED: f32 = 1.7;
const CHICK_FRESH_EGG_INITIAL_FALL_SPEED: f32 = 0.4;
const CHICK_FRESH_EGG_GRAVITY: f32 = 13.0;
const CHICK_FRESH_EGG_BASE_VISUAL_SCALE: f32 = 0.46;
const CHICK_FRESH_EGG_BASE_RADIUS: f32 = 0.38;
const CHICK_FRESH_EGG_SIZE_MULTIPLIER: f32 = 3.0;
const CHICK_FRESH_EGG_VISUAL_SCALE: f32 =
    CHICK_FRESH_EGG_BASE_VISUAL_SCALE * CHICK_FRESH_EGG_SIZE_MULTIPLIER;
pub const CHICK_FRESH_EGG_LIFETIME: f32 = 1.0;
pub const CHICK_FRESH_EGG_RADIUS: f32 =
    CHICK_FRESH_EGG_BASE_RADIUS * CHICK_FRESH_EGG_SIZE_MULTIPLIER;
pub const CHICK_FRESH_EGG_RIDE_LIFETIME: f32 = 0.56;
const CHICK_FRESH_EGG_RIDE_FORWARD_OFFSET: f32 = 0.22;
const CHICK_FRESH_EGG_RIDE_VERTICAL_OFFSET: f32 = 0.18;
#[cfg(all(test, target_os = "macos", target_arch = "aarch64"))]
const CHICK_FRESH_EGG_RIDE_BOB_HEIGHT: f32 = 0.04;
const CHICK_EGGPLANT_SPEED: f32 = 4.6;
pub const CHICK_EGGPLANT_LIFETIME: f32 = 1.22;
pub const CHICK_EGGPLANT_RADIUS: f32 = 0.46;
pub const CHICK_SUNNY_SPLASH_LIFETIME: f32 = 1.15;
pub const CHICK_SUNNY_SPLASH_RADIUS: f32 = 0.84;
pub const CHICK_SUNNY_SPLASH_TICK: f32 = 0.36;
const CHICK_SUNNY_SPLASH_VERTICAL_REACH: f32 = 0.44;
pub const CHICK_OMELET_FIELD_LIFETIME: f32 = 2.05;
pub const CHICK_OMELET_FIELD_RADIUS: f32 = 1.55;
pub const CHICK_OMELET_FIELD_TICK: f32 = 0.42;
const CHICK_OMELET_FIELD_VERTICAL_REACH: f32 = 1.1;
const CHICK_OMELET_FIELD_CENTER_OFFSET: f32 = 1.75;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChickSkillKind {
    ShellChip,
    FriedEggDisc,
    EggCupMortar,
    OrbitEgg,
    OrbitEggLaunch,
    OrbitEggReturn,
    FreshEggDrop,
    FreshEggRide,
    EggplantRoll,
    SunnySplash,
    OmeletField,
}

#[derive(Component)]
pub struct ActiveChickSkill {
    pub kind: ChickSkillKind,
    pub owner: FighterId,
    pub owner_style: FighterStyleKind,
    pub payload_id: Option<AttackPayloadId>,
    pub shape_id: AttackShapeId,
    pub source: ImpactSource,
    pub facing: Vec3,
    pub velocity: Vec3,
    pub lifetime: TickTimer,
    pub age: ElapsedTicks,
    pub radius: f32,
    pub guard_stamina_damage: f32,
    pub repeat_interval: Option<TickTimer>,
    pub repeat_timer: Option<TickTimer>,
    pub already_hit: FighterHitMask,
    pub size_scale: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ActiveChickSkillSnapshot {
    pub id: SimEntityId,
    pub owner: FighterId,
    pub kind: ChickSkillKind,
    pub position: Vec3,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ChickPresentationKind {
    Lifecycle {
        event: AbilityLifecycleEvent,
        position: Vec3,
        direction: Vec3,
        package: Option<FeedbackPackageId>,
        cue: Option<&'static str>,
        source: ImpactSource,
        priority: u8,
        hud_flash: Option<(FighterId, f32)>,
    },
    Impact {
        victim: FighterId,
        position: Vec3,
        direction: Vec3,
        package: FeedbackPackageId,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ChickPresentationIntent {
    pub event_id: SimEventId,
    pub entity: SimEntityId,
    pub kind: ChickPresentationKind,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ChickPresentationIntentSlot {
    tick: SimTick,
    len: u16,
    occupied: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChickPresentationIntentMetrics {
    pub recorded: u64,
    pub replaced: u64,
    pub rejected: u64,
    pub discarded: u64,
}

/// Fixed-capacity render-only sidecar keyed by deterministic simulation event IDs.
#[derive(Resource, Clone, Debug)]
pub struct ChickPresentationIntentJournal {
    slots: [ChickPresentationIntentSlot; SIM_EVENT_HISTORY_TICKS],
    intents: Box<[Option<ChickPresentationIntent>]>,
    len: usize,
    metrics: ChickPresentationIntentMetrics,
}

impl Default for ChickPresentationIntentJournal {
    fn default() -> Self {
        Self {
            slots: [ChickPresentationIntentSlot::default(); SIM_EVENT_HISTORY_TICKS],
            intents: vec![None; SIM_EVENT_HISTORY_TICKS * MAX_SIM_EVENTS_PER_TICK]
                .into_boxed_slice(),
            len: 0,
            metrics: ChickPresentationIntentMetrics::default(),
        }
    }
}

impl ChickPresentationIntentJournal {
    const fn slot_index(tick: SimTick) -> usize {
        tick.0 as usize % SIM_EVENT_HISTORY_TICKS
    }

    const fn slot_offset(slot: usize) -> usize {
        slot * MAX_SIM_EVENTS_PER_TICK
    }

    #[cfg(test)]
    pub const fn len(&self) -> usize {
        self.len
    }

    #[cfg(test)]
    pub const fn capacity(&self) -> usize {
        SIM_EVENT_HISTORY_TICKS * MAX_SIM_EVENTS_PER_TICK
    }

    #[cfg(test)]
    pub const fn metrics(&self) -> ChickPresentationIntentMetrics {
        self.metrics
    }

    pub(crate) fn record(&mut self, intent: ChickPresentationIntent) -> Result<(), EventEmitError> {
        let ordinal = usize::from(intent.event_id.ordinal);
        if ordinal >= MAX_SIM_EVENTS_PER_TICK {
            self.metrics.rejected = self.metrics.rejected.saturating_add(1);
            return Err(EventEmitError::CapacityExceeded {
                capacity: MAX_SIM_EVENTS_PER_TICK,
            });
        }

        let slot_index = Self::slot_index(intent.event_id.tick);
        let offset = Self::slot_offset(slot_index);
        let slot = &mut self.slots[slot_index];
        if slot.occupied && slot.tick != intent.event_id.tick {
            for entry in &mut self.intents[offset..offset + MAX_SIM_EVENTS_PER_TICK] {
                *entry = None;
            }
            self.len = self.len.saturating_sub(usize::from(slot.len));
        }
        if !slot.occupied || slot.tick != intent.event_id.tick {
            *slot = ChickPresentationIntentSlot {
                tick: intent.event_id.tick,
                len: 0,
                occupied: true,
            };
        }

        let entry = &mut self.intents[offset + ordinal];
        if entry.is_some() {
            self.metrics.replaced = self.metrics.replaced.saturating_add(1);
        } else {
            slot.len += 1;
            self.len += 1;
        }
        *entry = Some(intent);
        self.metrics.recorded = self.metrics.recorded.saturating_add(1);
        Ok(())
    }

    pub(crate) fn get(&self, event_id: SimEventId) -> Option<ChickPresentationIntent> {
        let ordinal = usize::from(event_id.ordinal);
        if ordinal >= MAX_SIM_EVENTS_PER_TICK {
            return None;
        }
        let slot_index = Self::slot_index(event_id.tick);
        let slot = self.slots[slot_index];
        if !slot.occupied || slot.tick != event_id.tick {
            return None;
        }
        self.intents[Self::slot_offset(slot_index) + ordinal]
            .filter(|intent| intent.event_id == event_id)
    }

    pub fn discard_after(&mut self, retained_through: SimTick) {
        for slot_index in 0..SIM_EVENT_HISTORY_TICKS {
            let slot = self.slots[slot_index];
            if !slot.occupied || slot.tick <= retained_through {
                continue;
            }
            let offset = Self::slot_offset(slot_index);
            for entry in &mut self.intents[offset..offset + MAX_SIM_EVENTS_PER_TICK] {
                *entry = None;
            }
            self.slots[slot_index] = ChickPresentationIntentSlot::default();
            self.len = self.len.saturating_sub(usize::from(slot.len));
            self.metrics.discarded = self.metrics.discarded.saturating_add(u64::from(slot.len));
        }
    }
}

impl RollbackEventDiscard for ChickPresentationIntentJournal {
    fn discard_after(&mut self, retained_through: SimTick) {
        Self::discard_after(self, retained_through);
    }
}

fn chick_presentation_matches_event(event: SimEvent, intent: ChickPresentationIntent) -> bool {
    if event.id != intent.event_id || event.id.source != SimEventSource::Entity(intent.entity) {
        return false;
    }
    match intent.kind {
        ChickPresentationKind::Lifecycle {
            event: expected, ..
        } => matches!(
            event.kind,
            SimEventKind::AbilityLifecycle { entity, event }
                if entity == intent.entity && event == expected
        ),
        ChickPresentationKind::Impact { victim, .. } => {
            matches!(
                event.kind,
                SimEventKind::HitConfirmed { victim: event_victim, .. }
                    if event_victim == victim
            ) || matches!(
                event.kind,
                SimEventKind::Guarded { defender, .. } if defender == victim
            )
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct ChickPresentationResult {
    pub presented: bool,
    pub hud_flash: Option<(FighterId, f32)>,
}

/// Applies a validated render-only Chick sidecar from the shared event router.
pub(crate) fn present_chick_event(
    event: SimEvent,
    intent: ChickPresentationIntent,
    commands: &mut Commands,
    effect_assets: &EffectAssets,
    feedback: &mut HitEffects,
) -> ChickPresentationResult {
    if !chick_presentation_matches_event(event, intent) {
        return ChickPresentationResult::default();
    }

    let hud_flash = match intent.kind {
        ChickPresentationKind::Lifecycle {
            position,
            direction,
            package,
            cue,
            source,
            priority,
            hud_flash,
            ..
        } => {
            if let Some(package) = package {
                spawn_feedback_package(commands, effect_assets, position, direction, package);
            }
            if let Some(cue) = cue {
                feedback.push_feedback_cue(cue, source, priority);
            }
            hud_flash
        }
        ChickPresentationKind::Impact {
            position,
            direction,
            package,
            ..
        } => {
            spawn_feedback_package(commands, effect_assets, position, direction, package);
            None
        }
    };
    ChickPresentationResult {
        presented: true,
        hud_flash,
    }
}

/// Bounded semantic-event boundary for canonical Chick simulation. Dedicated
/// authority worlds omit both optional render journals.
#[derive(SystemParam)]
pub(crate) struct ChickPresentationEmitter<'w> {
    sim_events: ResMut<'w, TickEventBuffer>,
    chick_intents: Option<ResMut<'w, ChickPresentationIntentJournal>>,
}

impl ChickPresentationEmitter<'_> {
    #[allow(clippy::too_many_arguments)]
    fn emit_lifecycle(
        &mut self,
        entity: SimEntityId,
        event: AbilityLifecycleEvent,
        position: Vec3,
        direction: Vec3,
        package: Option<FeedbackPackageId>,
        cue: Option<&'static str>,
        source: ImpactSource,
        priority: u8,
        hud_flash: Option<(FighterId, f32)>,
    ) {
        let Ok(event_id) = self.sim_events.emit(
            SimEventSource::Entity(entity),
            SimEventKind::AbilityLifecycle { entity, event },
        ) else {
            return;
        };
        if let Some(intents) = self.chick_intents.as_deref_mut() {
            let _ = intents.record(ChickPresentationIntent {
                event_id,
                entity,
                kind: ChickPresentationKind::Lifecycle {
                    event,
                    position,
                    direction,
                    package,
                    cue,
                    source,
                    priority,
                    hud_flash,
                },
            });
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn record_impact(
        &mut self,
        event_id: SimEventId,
        entity: SimEntityId,
        victim: FighterId,
        position: Vec3,
        direction: Vec3,
        package: FeedbackPackageId,
    ) {
        if let Some(intents) = self.chick_intents.as_deref_mut() {
            let _ = intents.record(ChickPresentationIntent {
                event_id,
                entity,
                kind: ChickPresentationKind::Impact {
                    victim,
                    position,
                    direction,
                    package,
                },
            });
        }
    }
}

#[derive(Resource, Default)]
pub struct ChickSkillAssets {
    egg_scene: Handle<Scene>,
    egg_half_scene: Handle<Scene>,
    egg_cooked_scene: Handle<Scene>,
    egg_cup_scene: Handle<Scene>,
    eggplant_scene: Handle<Scene>,
}

/// Render-only attachment marker. The canonical entity contains just stable
/// identity, translation, and [`ActiveChickSkill`]; clients rehydrate this
/// marker and its scene after rollback restore or late join.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ChickSkillVisualRoot {
    kind: ChickSkillKind,
}

pub fn setup_chick_skill_assets(mut commands: Commands, asset_server: Res<AssetServer>) {
    commands.insert_resource(ChickSkillAssets {
        egg_scene: asset_server.load(GltfAssetLabel::Scene(0).from_asset(CHICK_EGG_ASSET)),
        egg_half_scene: asset_server
            .load(GltfAssetLabel::Scene(0).from_asset(CHICK_EGG_HALF_ASSET)),
        egg_cooked_scene: asset_server
            .load(GltfAssetLabel::Scene(0).from_asset(CHICK_EGG_COOKED_ASSET)),
        egg_cup_scene: asset_server.load(GltfAssetLabel::Scene(0).from_asset(CHICK_EGG_CUP_ASSET)),
        eggplant_scene: asset_server
            .load(GltfAssetLabel::Scene(0).from_asset(CHICK_EGGPLANT_ASSET)),
    });
}

/// Rehydrates Chick render components after spawn, rollback restore, or late
/// join. Dedicated authority worlds never schedule this system or load these
/// assets.
pub fn attach_missing_chick_skill_visuals(
    mut commands: Commands,
    assets: Res<ChickSkillAssets>,
    skills: Query<
        (Entity, &ActiveChickSkill, &SimPosition, Option<&Transform>),
        Without<ChickSkillVisualRoot>,
    >,
) {
    for (entity, skill, position, transform) in &skills {
        if transform.is_none() {
            commands
                .entity(entity)
                .insert(Transform::from_translation(position.translation));
        }
        let (scene, name) = match skill.kind {
            ChickSkillKind::ShellChip => (assets.egg_half_scene.clone(), "Chick shell chip"),
            ChickSkillKind::FriedEggDisc => {
                (assets.egg_cooked_scene.clone(), "Chick fried egg disc")
            }
            ChickSkillKind::EggCupMortar => (assets.egg_cup_scene.clone(), "Chick egg-cup mortar"),
            ChickSkillKind::OrbitEgg => (assets.egg_scene.clone(), "Chick orbit egg"),
            ChickSkillKind::OrbitEggLaunch => {
                (assets.egg_scene.clone(), "Chick launched orbit egg")
            }
            ChickSkillKind::OrbitEggReturn => {
                (assets.egg_scene.clone(), "Chick returning orbit egg")
            }
            ChickSkillKind::FreshEggDrop => (assets.egg_scene.clone(), "Chick fresh egg drop"),
            ChickSkillKind::FreshEggRide => (assets.egg_scene.clone(), "Chick fresh egg ride"),
            ChickSkillKind::EggplantRoll => (assets.eggplant_scene.clone(), "Chick eggplant roll"),
            ChickSkillKind::SunnySplash => {
                (assets.egg_cooked_scene.clone(), "Chick sunny-side splash")
            }
            ChickSkillKind::OmeletField => (assets.egg_cooked_scene.clone(), "Chick omelet field"),
        };
        commands.entity(entity).insert((
            SceneRoot(scene),
            ChickSkillVisualRoot { kind: skill.kind },
            Name::new(name),
        ));
    }
}

/// Derives all presentation-only Chick rotation and scale from canonical state
/// during render Update. Snapshot/hash code intentionally ignores both fields.
pub fn sync_chick_skill_visuals(
    mut skills: Query<(
        &ActiveChickSkill,
        &mut ChickSkillVisualRoot,
        &SimPosition,
        &mut Transform,
    )>,
) {
    for (skill, mut visual, position, mut transform) in &mut skills {
        if visual.kind != skill.kind {
            // Orbit returns canonically become orbiters in place; both states
            // use the same egg scene, so only the render-mode marker changes.
            visual.kind = skill.kind;
        }
        transform.translation = position.translation;
        let age = skill.age.as_seconds();
        let ticks = skill.age.get() as f32;
        transform.scale = chick_skill_visual_scale(skill.kind, skill.size_scale, age);
        transform.rotation = match skill.kind {
            ChickSkillKind::ShellChip
            | ChickSkillKind::FriedEggDisc
            | ChickSkillKind::OrbitEggLaunch
            | ChickSkillKind::OrbitEggReturn
            | ChickSkillKind::EggplantRoll => {
                projectile_rotation(skill.facing) * Quat::from_rotation_y(ticks * 0.16)
            }
            ChickSkillKind::EggCupMortar => {
                projectile_rotation(skill.facing)
                    * Quat::from_rotation_x(ticks * 0.12)
                    * Quat::from_rotation_y(ticks * 0.08)
            }
            ChickSkillKind::OrbitEgg => orbit_egg_rotation(skill.facing, age),
            ChickSkillKind::FreshEggDrop => {
                projectile_rotation(skill.facing) * Quat::from_rotation_x(ticks * 0.1)
            }
            ChickSkillKind::FreshEggRide => projectile_rotation(skill.facing),
            ChickSkillKind::SunnySplash | ChickSkillKind::OmeletField => Quat::IDENTITY,
        };
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_chick_skill_with_presentation(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    presentation: &mut ChickPresentationEmitter,
    state: &MatchState,
    arena: &ArenaDefinition,
    owner: FighterId,
    owner_id: usize,
    owner_style: FighterStyleKind,
    origin: Vec3,
    facing: Vec3,
    aim_held: bool,
    owner_size_scale: f32,
    skill: ChickSkillId,
    targets: &[BeeSkillTargetSnapshot],
    active_skills: &[ActiveChickSkillSnapshot],
) {
    let mut presentation = Some(presentation);
    spawn_chick_skill_canonical(
        commands,
        identities,
        &mut presentation,
        state,
        arena,
        owner,
        owner_id,
        owner_style,
        origin,
        facing,
        aim_held,
        owner_size_scale,
        skill,
        targets,
        active_skills,
    );
}

#[allow(clippy::too_many_arguments)]
fn spawn_chick_skill_canonical(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    presentation: &mut Option<&mut ChickPresentationEmitter>,
    state: &MatchState,
    arena: &ArenaDefinition,
    owner: FighterId,
    owner_id: usize,
    owner_style: FighterStyleKind,
    origin: Vec3,
    facing: Vec3,
    aim_held: bool,
    owner_size_scale: f32,
    skill: ChickSkillId,
    targets: &[BeeSkillTargetSnapshot],
    active_skills: &[ActiveChickSkillSnapshot],
) {
    let facing = normalized_or_forward(facing);
    let size_scale = chick_skill_size_scale(owner_size_scale);
    let target = chick_skill_lock_target(owner, origin, facing, aim_held, state, targets);
    match skill {
        ChickSkillId::ShellPeck => spawn_shell_chip_pair(
            commands,
            identities,
            presentation,
            owner,
            owner_id,
            owner_style,
            origin + Vec3::Y * 0.92 * size_scale + facing * 0.48 * size_scale,
            facing,
            target,
            targets,
            size_scale,
        ),
        ChickSkillId::SunnyFlip => {
            let spawn = origin + (Vec3::Y * 0.92 + facing * 0.54) * size_scale;
            let direction = target
                .and_then(|entity| target_position(entity, targets))
                .map(|position| flat_direction(spawn, position))
                .filter(|direction| canonical_math::vec3_length_squared(*direction) > 0.01)
                .unwrap_or(facing);
            spawn_fried_egg_disc(
                commands,
                identities,
                presentation,
                owner,
                owner_id,
                owner_style,
                spawn,
                direction,
                size_scale,
            );
        }
        ChickSkillId::ShellScramble => spawn_shell_chip_fan(
            commands,
            identities,
            presentation,
            owner,
            owner_id,
            owner_style,
            origin + (Vec3::Y * 0.55 + facing * 0.72) * size_scale,
            facing,
            size_scale,
            true,
        ),
        ChickSkillId::EggCupMortar => {
            let spawn = origin + (Vec3::Y * 1.05 + facing * 0.45) * size_scale;
            let direction = target
                .and_then(|entity| target_position(entity, targets))
                .map(|position| flat_direction(spawn, position))
                .filter(|direction| canonical_math::vec3_length_squared(*direction) > 0.01)
                .unwrap_or(facing);
            spawn_egg_cup_mortar(
                commands,
                identities,
                presentation,
                owner,
                owner_id,
                owner_style,
                spawn,
                direction,
                size_scale,
            );
        }
        ChickSkillId::OrbitEgg => {
            replace_owner_orbit_eggs(commands, identities, presentation, owner, active_skills);
            spawn_orbit_egg(
                commands,
                identities,
                presentation,
                owner,
                owner_id,
                owner_style,
                origin,
                facing,
                size_scale,
            );
        }
        ChickSkillId::OrbitEggLaunch => {
            if let Some(orbit_egg) = owner_orbit_egg_for_launch(owner, active_skills) {
                replace_owner_orbit_eggs(commands, identities, presentation, owner, active_skills);
                spawn_orbit_egg_launch(
                    commands,
                    identities,
                    presentation,
                    owner,
                    owner_id,
                    owner_style,
                    orbit_egg.position,
                    facing,
                    size_scale,
                );
            } else {
                for (index, launched_egg) in
                    owner_launched_orbit_eggs_for_recall(owner, active_skills)
                        .into_iter()
                        .enumerate()
                {
                    if let Some(entity) = identities.mapped_entity(launched_egg.id) {
                        despawn_stable(
                            commands,
                            identities,
                            entity,
                            StableSimEntity::new(launched_egg.id),
                        );
                        emit_chick_lifecycle(
                            presentation,
                            launched_egg.id,
                            AbilityLifecycleEvent::Despawned,
                            launched_egg.position,
                            facing,
                            None,
                            None,
                            ImpactSource::Projectile,
                            0,
                            None,
                        );
                    }
                    spawn_orbit_egg_return(
                        commands,
                        identities,
                        presentation,
                        owner,
                        owner_id,
                        owner_style,
                        launched_egg.position,
                        facing,
                        size_scale,
                        index == 0,
                    );
                }
            }
        }
        ChickSkillId::UltimateEggBurst => {
            replace_owner_orbit_eggs(commands, identities, presentation, owner, active_skills);
            spawn_ultimate_egg_burst(
                commands,
                identities,
                presentation,
                owner,
                owner_id,
                owner_style,
                origin,
                facing,
                size_scale,
            );
        }
        ChickSkillId::EggplantRoll => {
            let spawn = grounded_position(
                arena,
                origin + facing * 0.58 * size_scale,
                0.19 * size_scale,
            );
            spawn_eggplant_roll(
                commands,
                identities,
                presentation,
                owner,
                owner_id,
                owner_style,
                spawn,
                facing,
                size_scale,
            );
        }
        ChickSkillId::FreshEggDrop => {
            let spawn = origin + (Vec3::Y * 0.28 + facing * 0.24) * size_scale;
            spawn_fresh_egg_drop(
                commands,
                identities,
                presentation,
                owner,
                owner_id,
                owner_style,
                spawn,
                facing,
                size_scale,
            );
        }
        ChickSkillId::FreshEggRide => spawn_fresh_egg_ride(
            commands,
            identities,
            presentation,
            owner,
            owner_id,
            owner_style,
            origin,
            facing,
            size_scale,
        ),
        ChickSkillId::SunnySideSplash => {
            let spawn = grounded_position(arena, origin + facing * 0.58 * size_scale, 0.04);
            spawn_sunny_splash(
                commands,
                identities,
                presentation,
                owner,
                owner_id,
                owner_style,
                spawn,
                facing,
                size_scale,
            );
        }
        ChickSkillId::OmeletField => {
            let spawn = chick_omelet_field_center(arena, origin, facing, size_scale);
            spawn_omelet_field(
                commands,
                identities,
                presentation,
                owner,
                owner_id,
                owner_style,
                spawn,
                facing,
                size_scale,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_shell_chip_pair(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    presentation: &mut Option<&mut ChickPresentationEmitter>,
    owner: FighterId,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    facing: Vec3,
    target: Option<FighterId>,
    targets: &[BeeSkillTargetSnapshot],
    size_scale: f32,
) {
    let side_vec = chick_skill_side_vec(facing);
    for (index, spread) in [-0.42, 0.42].into_iter().enumerate() {
        let spawn = position + side_vec * spread * 0.22 * size_scale;
        let direction = target
            .and_then(|entity| target_position(entity, targets))
            .map(|target| flat_direction(spawn, target))
            .filter(|direction| canonical_math::vec3_length_squared(*direction) > 0.01)
            .unwrap_or_else(|| canonical_math::vec3_normalize_or_zero(facing + side_vec * spread));
        spawn_shell_chip(
            commands,
            identities,
            presentation,
            owner,
            owner_id,
            owner_style,
            spawn,
            direction,
            size_scale,
            index == 0,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_shell_chip_fan(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    presentation: &mut Option<&mut ChickPresentationEmitter>,
    owner: FighterId,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    facing: Vec3,
    size_scale: f32,
    announce_cast: bool,
) {
    let side_vec = chick_skill_side_vec(facing);
    for (index, spread) in [-0.55, 0.0, 0.55].into_iter().enumerate() {
        let spawn = position + side_vec * spread * 0.24 * size_scale;
        let direction = canonical_math::vec3_normalize_or_zero(facing + side_vec * spread * 0.72);
        spawn_shell_chip(
            commands,
            identities,
            presentation,
            owner,
            owner_id,
            owner_style,
            spawn,
            direction,
            size_scale,
            announce_cast && index == 0,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_shell_chip(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    presentation: &mut Option<&mut ChickPresentationEmitter>,
    owner: FighterId,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    direction: Vec3,
    size_scale: f32,
    announce_cast: bool,
) {
    let facing = normalized_or_forward(direction);
    let Some((_, id)) = spawn_canonical_chick_skill(
        commands,
        identities,
        position,
        active_chick_skill(
            ChickSkillKind::ShellChip,
            owner,
            owner_id,
            owner_style,
            facing,
            facing * CHICK_SHELL_CHIP_SPEED,
            size_scale,
        ),
    ) else {
        return;
    };
    emit_chick_lifecycle(
        presentation,
        id,
        AbilityLifecycleEvent::Spawned,
        position,
        facing,
        Some(FeedbackPackageId::SpecialProjectileStartup),
        announce_cast.then_some("release_special_projectile"),
        ImpactSource::Projectile,
        if announce_cast { 24 } else { 0 },
        announce_cast.then_some((owner, 0.12)),
    );
}

#[allow(clippy::too_many_arguments)]
fn spawn_fried_egg_disc(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    presentation: &mut Option<&mut ChickPresentationEmitter>,
    owner: FighterId,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    direction: Vec3,
    size_scale: f32,
) {
    let facing = normalized_or_forward(direction);
    let Some((_, id)) = spawn_canonical_chick_skill(
        commands,
        identities,
        position,
        active_chick_skill(
            ChickSkillKind::FriedEggDisc,
            owner,
            owner_id,
            owner_style,
            facing,
            facing * CHICK_FRIED_DISC_SPEED,
            size_scale,
        ),
    ) else {
        return;
    };
    emit_chick_lifecycle(
        presentation,
        id,
        AbilityLifecycleEvent::Spawned,
        position,
        facing,
        Some(FeedbackPackageId::SpecialProjectileStartup),
        Some("release_special_projectile"),
        ImpactSource::Projectile,
        24,
        Some((owner, 0.12)),
    );
}

#[allow(clippy::too_many_arguments)]
fn spawn_egg_cup_mortar(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    presentation: &mut Option<&mut ChickPresentationEmitter>,
    owner: FighterId,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    direction: Vec3,
    size_scale: f32,
) {
    let facing = normalized_or_forward(direction);
    let velocity = facing * CHICK_EGG_CUP_SPEED + Vec3::Y * CHICK_EGG_CUP_LIFT;
    let Some((_, id)) = spawn_canonical_chick_skill(
        commands,
        identities,
        position,
        active_chick_skill(
            ChickSkillKind::EggCupMortar,
            owner,
            owner_id,
            owner_style,
            facing,
            velocity,
            size_scale,
        ),
    ) else {
        return;
    };
    emit_chick_lifecycle(
        presentation,
        id,
        AbilityLifecycleEvent::Spawned,
        position,
        facing,
        Some(FeedbackPackageId::SpecialProjectileStartup),
        Some("release_special_projectile"),
        ImpactSource::Projectile,
        24,
        Some((owner, 0.12)),
    );
}

fn spawn_canonical_chick_skill(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    position: Vec3,
    skill: ActiveChickSkill,
) -> Option<(Entity, SimEntityId)> {
    let entity = commands.spawn_empty().id();
    let stable = match identities.try_allocate(SimEntityKind::ChickSkill, entity) {
        Ok(stable) => stable,
        Err(_) => {
            commands.entity(entity).despawn();
            return None;
        }
    };
    commands
        .entity(entity)
        .insert((stable, SimPosition::new(position), skill));
    Some((entity, stable.id()))
}

#[allow(clippy::too_many_arguments)]
fn emit_chick_lifecycle(
    presentation: &mut Option<&mut ChickPresentationEmitter>,
    entity: SimEntityId,
    event: AbilityLifecycleEvent,
    position: Vec3,
    direction: Vec3,
    package: Option<FeedbackPackageId>,
    cue: Option<&'static str>,
    source: ImpactSource,
    priority: u8,
    hud_flash: Option<(FighterId, f32)>,
) {
    if let Some(presentation) = presentation.as_deref_mut() {
        presentation.emit_lifecycle(
            entity,
            entity_event(event),
            position,
            direction,
            package,
            cue,
            source,
            priority,
            hud_flash,
        );
    }
}

const fn entity_event(event: AbilityLifecycleEvent) -> AbilityLifecycleEvent {
    event
}

fn replace_owner_orbit_eggs(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    presentation: &mut Option<&mut ChickPresentationEmitter>,
    owner: FighterId,
    active_skills: &[ActiveChickSkillSnapshot],
) {
    for id in owner_orbit_egg_replacements(owner, active_skills) {
        if let Some(entity) = identities.mapped_entity(id) {
            despawn_stable(commands, identities, entity, StableSimEntity::new(id));
            let position = active_skills
                .iter()
                .find(|skill| skill.id == id)
                .map_or(Vec3::ZERO, |skill| skill.position);
            emit_chick_lifecycle(
                presentation,
                id,
                AbilityLifecycleEvent::Despawned,
                position,
                Vec3::ZERO,
                None,
                None,
                ImpactSource::Projectile,
                0,
                None,
            );
        }
    }
}

fn owner_orbit_egg_replacements(
    owner: FighterId,
    active_skills: &[ActiveChickSkillSnapshot],
) -> impl Iterator<Item = SimEntityId> + '_ {
    active_skills
        .iter()
        .filter(move |skill| {
            skill.owner == owner
                && matches!(
                    skill.kind,
                    ChickSkillKind::OrbitEgg
                        | ChickSkillKind::OrbitEggLaunch
                        | ChickSkillKind::OrbitEggReturn
                )
        })
        .map(|skill| skill.id)
}

fn owner_orbit_egg_for_launch(
    owner: FighterId,
    active_skills: &[ActiveChickSkillSnapshot],
) -> Option<ActiveChickSkillSnapshot> {
    active_skills
        .iter()
        .find(|skill| skill.owner == owner && skill.kind == ChickSkillKind::OrbitEgg)
        .copied()
}

fn owner_launched_orbit_eggs_for_recall(
    owner: FighterId,
    active_skills: &[ActiveChickSkillSnapshot],
) -> impl Iterator<Item = ActiveChickSkillSnapshot> + '_ {
    active_skills
        .iter()
        .filter(move |skill| skill.owner == owner && skill.kind == ChickSkillKind::OrbitEggLaunch)
        .copied()
}

#[allow(clippy::too_many_arguments)]
fn spawn_canonical_skill_with_feedback(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    presentation: &mut Option<&mut ChickPresentationEmitter>,
    position: Vec3,
    facing: Vec3,
    skill: ActiveChickSkill,
    package: FeedbackPackageId,
    announce_cast: bool,
) {
    let source = skill.source;
    let owner = skill.owner;
    let Some((_, id)) = spawn_canonical_chick_skill(commands, identities, position, skill) else {
        return;
    };
    emit_chick_lifecycle(
        presentation,
        id,
        AbilityLifecycleEvent::Spawned,
        position,
        facing,
        Some(package),
        announce_cast.then_some("release_special_projectile"),
        source,
        if announce_cast { 24 } else { 0 },
        announce_cast.then_some((owner, 0.12)),
    );
}

#[allow(clippy::too_many_arguments)]
fn spawn_orbit_egg(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    presentation: &mut Option<&mut ChickPresentationEmitter>,
    owner: FighterId,
    owner_id: usize,
    owner_style: FighterStyleKind,
    owner_position: Vec3,
    facing: Vec3,
    size_scale: f32,
) {
    let facing = normalized_or_forward(facing);
    let position = orbit_egg_position(owner_position, facing, size_scale, ElapsedTicks::ZERO);
    spawn_canonical_skill_with_feedback(
        commands,
        identities,
        presentation,
        position,
        facing,
        active_chick_skill(
            ChickSkillKind::OrbitEgg,
            owner,
            owner_id,
            owner_style,
            facing,
            Vec3::ZERO,
            size_scale,
        ),
        FeedbackPackageId::SpecialProjectileStartup,
        true,
    );
}

#[allow(clippy::too_many_arguments)]
fn spawn_orbit_egg_launch(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    presentation: &mut Option<&mut ChickPresentationEmitter>,
    owner: FighterId,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    direction: Vec3,
    size_scale: f32,
) {
    let facing = normalized_or_forward(direction);
    spawn_canonical_skill_with_feedback(
        commands,
        identities,
        presentation,
        position,
        facing,
        active_chick_skill(
            ChickSkillKind::OrbitEggLaunch,
            owner,
            owner_id,
            owner_style,
            facing,
            facing * CHICK_ORBIT_EGG_LAUNCH_SPEED,
            size_scale,
        ),
        FeedbackPackageId::SpecialProjectileStartup,
        true,
    );
}

#[allow(clippy::too_many_arguments)]
fn spawn_ultimate_egg_burst(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    presentation: &mut Option<&mut ChickPresentationEmitter>,
    owner: FighterId,
    owner_id: usize,
    owner_style: FighterStyleKind,
    origin: Vec3,
    facing: Vec3,
    size_scale: f32,
) {
    let facing = normalized_or_forward(facing);
    let origin = origin + Vec3::Y * CHICK_ORBIT_EGG_HEIGHT * size_scale;
    for (index, direction) in ultimate_egg_burst_directions(facing)
        .into_iter()
        .enumerate()
    {
        let position = origin + direction * CHICK_ULTIMATE_EGG_SPAWN_RADIUS * size_scale;
        let skill = ultimate_orbit_egg_skill(owner, owner_id, owner_style, direction, size_scale);
        let Some((_, id)) = spawn_canonical_chick_skill(commands, identities, position, skill)
        else {
            continue;
        };
        emit_chick_lifecycle(
            presentation,
            id,
            AbilityLifecycleEvent::Spawned,
            position,
            direction,
            (index == 0).then_some(FeedbackPackageId::SpecialProjectileStartup),
            (index == 0).then_some("release_special_projectile"),
            ImpactSource::Projectile,
            if index == 0 { 24 } else { 0 },
            (index == 0).then_some((owner, 0.12)),
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_orbit_egg_return(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    presentation: &mut Option<&mut ChickPresentationEmitter>,
    owner: FighterId,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    owner_facing: Vec3,
    size_scale: f32,
    announce_cast: bool,
) {
    let facing = normalized_or_forward(owner_facing);
    spawn_canonical_skill_with_feedback(
        commands,
        identities,
        presentation,
        position,
        facing,
        active_chick_skill(
            ChickSkillKind::OrbitEggReturn,
            owner,
            owner_id,
            owner_style,
            facing,
            Vec3::ZERO,
            size_scale,
        ),
        FeedbackPackageId::SpecialProjectileStartup,
        announce_cast,
    );
}

#[allow(clippy::too_many_arguments)]
fn spawn_fresh_egg_drop(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    presentation: &mut Option<&mut ChickPresentationEmitter>,
    owner: FighterId,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    facing: Vec3,
    size_scale: f32,
) {
    let facing = normalized_or_forward(facing);
    let velocity =
        facing * CHICK_FRESH_EGG_FORWARD_SPEED - Vec3::Y * CHICK_FRESH_EGG_INITIAL_FALL_SPEED;
    spawn_canonical_skill_with_feedback(
        commands,
        identities,
        presentation,
        position,
        facing,
        active_chick_skill(
            ChickSkillKind::FreshEggDrop,
            owner,
            owner_id,
            owner_style,
            facing,
            velocity,
            size_scale,
        ),
        FeedbackPackageId::SpecialProjectileStartup,
        true,
    );
}

#[allow(clippy::too_many_arguments)]
fn spawn_fresh_egg_ride(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    presentation: &mut Option<&mut ChickPresentationEmitter>,
    owner: FighterId,
    owner_id: usize,
    owner_style: FighterStyleKind,
    owner_position: Vec3,
    facing: Vec3,
    size_scale: f32,
) {
    let facing = normalized_or_forward(facing);
    let position = fresh_egg_ride_position(owner_position, facing, size_scale, ElapsedTicks::ZERO);
    spawn_canonical_skill_with_feedback(
        commands,
        identities,
        presentation,
        position,
        facing,
        active_chick_skill(
            ChickSkillKind::FreshEggRide,
            owner,
            owner_id,
            owner_style,
            facing,
            Vec3::ZERO,
            size_scale,
        ),
        FeedbackPackageId::SpecialProjectileStartup,
        true,
    );
}

#[allow(clippy::too_many_arguments)]
fn spawn_eggplant_roll(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    presentation: &mut Option<&mut ChickPresentationEmitter>,
    owner: FighterId,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    facing: Vec3,
    size_scale: f32,
) {
    let facing = normalized_or_forward(facing);
    spawn_canonical_skill_with_feedback(
        commands,
        identities,
        presentation,
        position,
        facing,
        active_chick_skill(
            ChickSkillKind::EggplantRoll,
            owner,
            owner_id,
            owner_style,
            facing,
            facing * CHICK_EGGPLANT_SPEED,
            size_scale,
        ),
        FeedbackPackageId::SpecialProjectileStartup,
        true,
    );
}

#[allow(clippy::too_many_arguments)]
fn spawn_sunny_splash(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    presentation: &mut Option<&mut ChickPresentationEmitter>,
    owner: FighterId,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    facing: Vec3,
    size_scale: f32,
) {
    let facing = normalized_or_forward(facing);
    spawn_canonical_skill_with_feedback(
        commands,
        identities,
        presentation,
        position,
        facing,
        active_chick_skill(
            ChickSkillKind::SunnySplash,
            owner,
            owner_id,
            owner_style,
            facing,
            Vec3::ZERO,
            size_scale,
        ),
        FeedbackPackageId::SpecialHazardStartup,
        true,
    );
}

#[allow(clippy::too_many_arguments)]
fn spawn_omelet_field(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    presentation: &mut Option<&mut ChickPresentationEmitter>,
    owner: FighterId,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    facing: Vec3,
    size_scale: f32,
) {
    let facing = normalized_or_forward(facing);
    spawn_canonical_skill_with_feedback(
        commands,
        identities,
        presentation,
        position,
        facing,
        active_chick_skill(
            ChickSkillKind::OmeletField,
            owner,
            owner_id,
            owner_style,
            facing,
            Vec3::ZERO,
            size_scale,
        ),
        FeedbackPackageId::SpecialHazardStartup,
        true,
    );
}

fn ultimate_orbit_egg_skill(
    owner: FighterId,
    owner_id: usize,
    owner_style: FighterStyleKind,
    facing: Vec3,
    size_scale: f32,
) -> ActiveChickSkill {
    debug_assert_eq!(owner.index(), owner_id);
    let facing = normalized_or_forward(facing);
    let mut skill = active_chick_skill(
        ChickSkillKind::OrbitEggLaunch,
        owner,
        owner_id,
        owner_style,
        facing,
        facing * CHICK_ORBIT_EGG_LAUNCH_SPEED,
        size_scale,
    );
    skill.lifetime = TickTimer::from_seconds_ceil(CHICK_ULTIMATE_EGG_LIFETIME);
    skill
}

fn ultimate_egg_burst_directions(facing: Vec3) -> [Vec3; CHICK_ULTIMATE_EGG_COUNT] {
    let facing = normalized_or_forward(facing);
    std::array::from_fn(|index| {
        let (cos, sin) = canonical_math::chick_ultimate_relative_basis(index);
        canonical_math::vec3_normalize_or_zero(Vec3::new(
            facing.x * cos - facing.z * sin,
            0.0,
            facing.z * cos + facing.x * sin,
        ))
    })
}

fn active_chick_skill(
    kind: ChickSkillKind,
    owner: FighterId,
    owner_id: usize,
    owner_style: FighterStyleKind,
    facing: Vec3,
    velocity: Vec3,
    size_scale: f32,
) -> ActiveChickSkill {
    debug_assert_eq!(owner.index(), owner_id);
    let size_scale = chick_skill_size_scale(size_scale);
    let (payload_id, shape_id, source, lifetime, radius, guard_stamina_damage, repeat_interval) =
        match kind {
            ChickSkillKind::ShellChip => (
                Some(AttackPayloadId::ChickShellChip),
                AttackShapeId::ProjectileBolt,
                ImpactSource::Projectile,
                CHICK_SHELL_CHIP_LIFETIME,
                CHICK_SHELL_CHIP_RADIUS,
                5.0,
                None,
            ),
            ChickSkillKind::FriedEggDisc => (
                Some(AttackPayloadId::ChickFriedEggDisc),
                AttackShapeId::ProjectileBolt,
                ImpactSource::Projectile,
                CHICK_FRIED_DISC_LIFETIME,
                CHICK_FRIED_DISC_RADIUS,
                8.0,
                None,
            ),
            ChickSkillKind::EggCupMortar => (
                Some(AttackPayloadId::ChickEggCupMortar),
                AttackShapeId::ProjectileBolt,
                ImpactSource::Projectile,
                CHICK_EGG_CUP_LIFETIME,
                CHICK_EGG_CUP_RADIUS,
                11.0,
                None,
            ),
            ChickSkillKind::OrbitEgg => (
                Some(AttackPayloadId::ChickOrbitEgg),
                AttackShapeId::ProjectileBolt,
                ImpactSource::Projectile,
                CHICK_ORBIT_EGG_LIFETIME,
                CHICK_ORBIT_EGG_RADIUS,
                2.0,
                None,
            ),
            ChickSkillKind::OrbitEggLaunch => (
                Some(AttackPayloadId::ChickOrbitEgg),
                AttackShapeId::ProjectileBolt,
                ImpactSource::Projectile,
                CHICK_ORBIT_EGG_LAUNCH_LIFETIME,
                CHICK_ORBIT_EGG_LAUNCH_RADIUS,
                2.0,
                None,
            ),
            ChickSkillKind::OrbitEggReturn => (
                Some(AttackPayloadId::ChickOrbitEggLaunch),
                AttackShapeId::ProjectileBolt,
                ImpactSource::Projectile,
                CHICK_ORBIT_EGG_RETURN_LIFETIME,
                CHICK_ORBIT_EGG_RETURN_RADIUS,
                10.0,
                None,
            ),
            ChickSkillKind::FreshEggDrop => (
                Some(AttackPayloadId::ChickFreshEggDrop),
                AttackShapeId::ProjectileBolt,
                ImpactSource::Projectile,
                CHICK_FRESH_EGG_LIFETIME,
                CHICK_FRESH_EGG_RADIUS,
                7.0,
                None,
            ),
            ChickSkillKind::FreshEggRide => (
                None,
                AttackShapeId::ProjectileBolt,
                ImpactSource::Projectile,
                CHICK_FRESH_EGG_RIDE_LIFETIME,
                0.0,
                0.0,
                None,
            ),
            ChickSkillKind::EggplantRoll => (
                Some(AttackPayloadId::ChickEggplantRoll),
                AttackShapeId::ProjectileBolt,
                ImpactSource::Projectile,
                CHICK_EGGPLANT_LIFETIME,
                CHICK_EGGPLANT_RADIUS,
                12.0,
                None,
            ),
            ChickSkillKind::SunnySplash => (
                Some(AttackPayloadId::ChickSunnySplash),
                AttackShapeId::HazardField,
                ImpactSource::Hazard,
                CHICK_SUNNY_SPLASH_LIFETIME,
                CHICK_SUNNY_SPLASH_RADIUS,
                5.0,
                Some(CHICK_SUNNY_SPLASH_TICK),
            ),
            ChickSkillKind::OmeletField => (
                Some(AttackPayloadId::ChickOmeletField),
                AttackShapeId::HazardField,
                ImpactSource::Hazard,
                CHICK_OMELET_FIELD_LIFETIME,
                CHICK_OMELET_FIELD_RADIUS,
                6.0,
                Some(CHICK_OMELET_FIELD_TICK),
            ),
        };

    ActiveChickSkill {
        kind,
        owner,
        owner_style,
        payload_id,
        shape_id,
        source,
        facing: normalized_or_forward(facing),
        velocity,
        lifetime: TickTimer::from_seconds_ceil(lifetime),
        age: ElapsedTicks::ZERO,
        radius: radius * size_scale,
        guard_stamina_damage,
        repeat_interval: repeat_interval.map(TickTimer::from_seconds_ceil),
        repeat_timer: repeat_interval.map(TickTimer::from_seconds_ceil),
        already_hit: FighterHitMask::default(),
        size_scale,
    }
}

fn chick_skill_size_scale(owner_size_scale: f32) -> f32 {
    owner_size_scale.max(0.1)
}

fn chick_skill_visual_scale(kind: ChickSkillKind, size_scale: f32, age: f32) -> Vec3 {
    let size_scale = chick_skill_size_scale(size_scale);
    match kind {
        ChickSkillKind::ShellChip => Vec3::splat(0.38 * size_scale),
        ChickSkillKind::FriedEggDisc => Vec3::splat(0.48 * size_scale),
        ChickSkillKind::EggCupMortar => Vec3::splat(0.5 * size_scale),
        ChickSkillKind::OrbitEgg
        | ChickSkillKind::OrbitEggLaunch
        | ChickSkillKind::OrbitEggReturn => {
            Vec3::splat(0.44 * CHICK_ORBIT_EGG_VISUAL_SCALE * size_scale)
        }
        ChickSkillKind::FreshEggDrop | ChickSkillKind::FreshEggRide => {
            Vec3::splat(CHICK_FRESH_EGG_VISUAL_SCALE * size_scale)
        }
        ChickSkillKind::EggplantRoll => Vec3::splat(0.58 * size_scale),
        ChickSkillKind::SunnySplash => Vec3::splat(sunny_splash_visual_pulse(age) * size_scale),
        ChickSkillKind::OmeletField => Vec3::splat(omelet_field_visual_pulse(age) * size_scale),
    }
}

fn sunny_splash_visual_pulse(age: f32) -> f32 {
    0.78 + (age * 9.0).sin().abs() * 0.08
}

fn omelet_field_visual_pulse(age: f32) -> f32 {
    1.28 + (age * 12.0).sin().abs() * 0.12
}

fn sunny_splash_canonical_scale(age: ElapsedTicks) -> f32 {
    crate::canonical_math::chick_sunny_splash_scale(age.get())
}

fn omelet_field_canonical_scale(age: ElapsedTicks) -> f32 {
    crate::canonical_math::chick_omelet_field_scale(age.get())
}

pub fn collect_chick_skill_contacts(
    identities: Res<SimulationIdentityAllocator>,
    state: Res<MatchState>,
    feel: Res<CombatFeelTuning>,
    hitstop: Res<Hitstop>,
    mut contact_buffer: ResMut<ContactBuffer>,
    mut presentation: ChickPresentationEmitter,
    mut skills: Query<
        (&StableSimEntity, &mut ActiveChickSkill, &mut SimPosition),
        Without<Fighter>,
    >,
    mut fighters: ParamSet<(
        Query<(&Fighter, &FighterActionState, &SimPosition), With<Fighter>>,
        Query<
            (
                &Fighter,
                &FighterStats,
                &FighterMotor,
                &FighterActionState,
                &SimPosition,
            ),
            With<Fighter>,
        >,
    )>,
) {
    if hitstop.active() {
        return;
    }

    let dt = 1.0 / SIM_HZ_U32 as f32;
    for index in 0..identities.capacity(SimEntityKind::ChickSkill) {
        let Some((skill_id, skill_entity)) = identities.entry_at(SimEntityKind::ChickSkill, index)
        else {
            continue;
        };
        let Ok((stable, mut skill, mut transform)) = skills.get_mut(skill_entity) else {
            continue;
        };
        if stable.id() != skill_id {
            continue;
        }
        skill.age.advance();
        skill.lifetime.tick();
        if update_skill_repeat_window(&mut skill) {
            presentation.emit_lifecycle(
                skill_id,
                AbilityLifecycleEvent::Repeated,
                transform.translation,
                skill.facing,
                None,
                Some("pulse_chick_breakfast_hazard"),
                skill.source,
                24,
                None,
            );
        }
        update_chick_skill_motion(&mut skill, &mut transform, dt, &fighters.p0());

        {
            let target_fighters = fighters.p1();
            for target_id in FighterId::ALL {
                let Some((_target, stats, _motor, action, target_transform)) = target_fighters
                    .iter()
                    .find(|(fighter, ..)| fighter.id == target_id.index())
                else {
                    continue;
                };
                if !chick_skill_can_hit_target(&skill, target_id, &state) {
                    continue;
                }
                if (chick_skill_uses_hit_memory(skill.kind)
                    && skill.already_hit.contains(target_id))
                    || !can_receive_impact(&stats, &action)
                    || !chick_skill_overlaps_target(&skill, transform.translation, target_transform)
                {
                    continue;
                }

                let profile = chick_skill_impact_profile(&skill, &feel);
                let _ = contact_buffer.push(ContactRecord::new(
                    ContactPhase::Strike,
                    ContactSourceKind::CharacterAbility,
                    skill_id,
                    Some(skill.owner),
                    target_id,
                    skill.payload_id.map_or(u16::MAX, |payload| payload as u16),
                    skill.shape_id as u16,
                    0,
                    target_transform.translation,
                    transform.translation,
                    profile,
                    ContactFlags::default(),
                ));
            }
        }
    }
}

pub fn apply_chick_skill_contact_outcomes(
    mut commands: Commands,
    mut identities: ResMut<SimulationIdentityAllocator>,
    active_arena: Res<ActiveArena>,
    mut contact_buffer: ResMut<ContactBuffer>,
    mut presentation: ChickPresentationEmitter,
    mut skills: Query<(&StableSimEntity, &mut ActiveChickSkill, &SimPosition), Without<Fighter>>,
) {
    for contact_index in 0..contact_buffer.len() {
        let Some(contact) = contact_buffer.record(contact_index) else {
            continue;
        };
        if contact.source_kind != ContactSourceKind::CharacterAbility {
            continue;
        }
        let Some(source) = contact.source.entity() else {
            continue;
        };
        if source.kind() != SimEntityKind::ChickSkill {
            continue;
        }
        let Some(skill_entity) = identities.mapped_entity(source) else {
            contact_buffer.mark_outcome(contact_index, ContactOutcomeKind::Invalidated);
            continue;
        };
        let Ok((stable, mut skill, _)) = skills.get_mut(skill_entity) else {
            contact_buffer.mark_outcome(contact_index, ContactOutcomeKind::Invalidated);
            continue;
        };
        if stable.id() != source {
            contact_buffer.mark_outcome(contact_index, ContactOutcomeKind::Invalidated);
            continue;
        }
        let Some(outcome) = contact_buffer.outcome(contact_index) else {
            continue;
        };
        if !matches!(
            outcome.kind,
            ContactOutcomeKind::Accepted | ContactOutcomeKind::Guarded
        ) {
            continue;
        }

        if chick_skill_uses_hit_memory(skill.kind) {
            skill.already_hit.insert(contact.target);
        }
        if chick_skill_consumed_on_hit(skill.kind) {
            skill.lifetime.clear();
        }
        if let Some(event_id) = outcome.event_id {
            presentation.record_impact(
                event_id,
                source,
                contact.target,
                contact.contact_point.to_vec3() + Vec3::Y * (FIGHTER_HEIGHT * 0.58),
                skill.facing,
                impact_package(skill.kind),
            );
        }
    }

    for index in 0..identities.capacity(SimEntityKind::ChickSkill) {
        let Some((skill_id, skill_entity)) = identities.entry_at(SimEntityKind::ChickSkill, index)
        else {
            continue;
        };
        let Ok((stable, skill, transform)) = skills.get_mut(skill_entity) else {
            continue;
        };
        if stable.id() != skill_id {
            continue;
        }
        let hit_this_tick = (0..contact_buffer.len()).any(|contact_index| {
            contact_buffer
                .record(contact_index)
                .filter(|contact| contact.source.entity() == Some(skill_id))
                .and_then(|_| contact_buffer.outcome(contact_index))
                .is_some_and(|outcome| {
                    matches!(
                        outcome.kind,
                        ContactOutcomeKind::Accepted | ContactOutcomeKind::Guarded
                    )
                })
        });

        let cracked =
            fresh_egg_drop_touched_ground(&skill, transform.translation, active_arena.definition());
        let projectile_grounded = chick_projectile_touched_ground(
            &skill,
            transform.translation,
            active_arena.definition(),
        );
        if cracked {
            let mut skill_presentation = Some(&mut presentation);
            spawn_shell_chip_fan(
                &mut commands,
                &mut identities,
                &mut skill_presentation,
                skill.owner,
                skill.owner.index(),
                skill.owner_style,
                transform.translation + Vec3::Y * 0.16,
                skill.facing,
                skill.size_scale,
                false,
            );
            presentation.emit_lifecycle(
                skill_id,
                AbilityLifecycleEvent::Despawned,
                transform.translation,
                skill.facing,
                Some(FeedbackPackageId::SpecialProjectileImpact),
                None,
                skill.source,
                0,
                None,
            );
        }

        if !skill.lifetime.active()
            || cracked
            || projectile_grounded
            || should_despawn_skill(transform.translation, active_arena.definition())
        {
            if !hit_this_tick && !cracked {
                presentation.emit_lifecycle(
                    skill_id,
                    AbilityLifecycleEvent::Despawned,
                    transform.translation,
                    skill.facing,
                    Some(despawn_package(skill.kind)),
                    None,
                    skill.source,
                    0,
                    None,
                );
            }
            despawn_stable(&mut commands, &mut identities, skill_entity, *stable);
        }
    }
}

fn update_skill_repeat_window(skill: &mut ActiveChickSkill) -> bool {
    let Some(interval) = skill.repeat_interval else {
        return false;
    };
    let Some(mut repeat_timer) = skill.repeat_timer else {
        return false;
    };
    let repeated = repeat_timer.tick();
    if repeated {
        skill.already_hit.clear();
        repeat_timer.set(interval);
    }
    skill.repeat_timer = Some(repeat_timer);
    repeated
}

fn update_chick_skill_motion(
    skill: &mut ActiveChickSkill,
    transform: &mut SimPosition,
    dt: f32,
    owners: &Query<(&Fighter, &FighterActionState, &SimPosition), With<Fighter>>,
) {
    match skill.kind {
        ChickSkillKind::ShellChip
        | ChickSkillKind::FriedEggDisc
        | ChickSkillKind::OrbitEggLaunch
        | ChickSkillKind::OrbitEggReturn
        | ChickSkillKind::EggplantRoll => {
            if skill.kind == ChickSkillKind::OrbitEggReturn {
                let owner_position = owners
                    .iter()
                    .find(|(fighter, ..)| fighter.id == skill.owner.index())
                    .map(|(_, _, owner_transform)| owner_transform.translation);
                update_orbit_egg_return_motion(skill, transform, owner_position, dt);
            } else {
                transform.translation += skill.velocity * dt;
            }
        }
        ChickSkillKind::EggCupMortar => {
            skill.velocity.y -= CHICK_EGG_CUP_GRAVITY * dt;
            transform.translation += skill.velocity * dt;
        }
        ChickSkillKind::OrbitEgg => {
            let owner_position = owners
                .iter()
                .find(|(fighter, ..)| fighter.id == skill.owner.index())
                .map(|(_, _, owner_transform)| owner_transform.translation);
            update_orbit_egg_motion(skill, transform, owner_position);
        }
        ChickSkillKind::FreshEggDrop => {
            skill.velocity.y -= CHICK_FRESH_EGG_GRAVITY * dt;
            transform.translation += skill.velocity * dt;
        }
        ChickSkillKind::FreshEggRide => {
            let owner_state = owners
                .iter()
                .find(|(fighter, ..)| fighter.id == skill.owner.index())
                .map(|(_, action, transform)| (action, transform));
            update_fresh_egg_ride_motion(skill, transform, owner_state);
        }
        ChickSkillKind::SunnySplash | ChickSkillKind::OmeletField => {}
    }
}

fn update_orbit_egg_motion(
    skill: &mut ActiveChickSkill,
    transform: &mut SimPosition,
    owner_position: Option<Vec3>,
) {
    let Some(owner_position) = owner_position else {
        skill.lifetime.clear();
        return;
    };
    transform.translation =
        orbit_egg_position(owner_position, skill.facing, skill.size_scale, skill.age);
}

fn update_orbit_egg_return_motion(
    skill: &mut ActiveChickSkill,
    transform: &mut SimPosition,
    owner_position: Option<Vec3>,
    dt: f32,
) {
    let Some(owner_position) = owner_position else {
        skill.lifetime.clear();
        return;
    };
    let target = orbit_egg_position(
        owner_position,
        skill.facing,
        skill.size_scale,
        ElapsedTicks::ZERO,
    );
    let to_target = target - transform.translation;
    let distance = canonical_math::vec3_distance(target, transform.translation);
    if distance <= CHICK_ORBIT_EGG_RETURN_ARRIVAL_DISTANCE {
        restore_returned_orbit_egg(skill, transform, target);
        return;
    }

    let travel = CHICK_ORBIT_EGG_RETURN_SPEED * dt;
    if travel >= distance {
        transform.translation = target;
        restore_returned_orbit_egg(skill, transform, target);
        return;
    }

    let direction = to_target / distance;
    skill.velocity = direction * CHICK_ORBIT_EGG_RETURN_SPEED;
    transform.translation += skill.velocity * dt;
}

fn restore_returned_orbit_egg(
    skill: &mut ActiveChickSkill,
    transform: &mut SimPosition,
    target: Vec3,
) {
    let owner = skill.owner;
    let owner_id = skill.owner.index();
    let owner_style = skill.owner_style;
    let facing = skill.facing;
    let size_scale = skill.size_scale;
    *skill = active_chick_skill(
        ChickSkillKind::OrbitEgg,
        owner,
        owner_id,
        owner_style,
        facing,
        Vec3::ZERO,
        size_scale,
    );
    transform.translation = target;
}

fn update_fresh_egg_ride_motion(
    skill: &mut ActiveChickSkill,
    transform: &mut SimPosition,
    owner_state: Option<(&FighterActionState, &SimPosition)>,
) {
    let Some((owner_action, owner_transform)) = owner_state else {
        skill.lifetime.clear();
        return;
    };
    if owner_action.technique_id != Some(TechniqueId::ChickJumpHeavy) {
        skill.lifetime.clear();
        return;
    }

    transform.translation = fresh_egg_ride_position(
        owner_transform.translation,
        skill.facing,
        skill.size_scale,
        skill.age,
    );
}

fn fresh_egg_ride_position(
    owner_position: Vec3,
    facing: Vec3,
    size_scale: f32,
    age: ElapsedTicks,
) -> Vec3 {
    let size_scale = chick_skill_size_scale(size_scale);
    let bob = crate::canonical_math::chick_fresh_ride_bob(age.get()) * size_scale;
    owner_position
        + normalized_or_forward(facing) * CHICK_FRESH_EGG_RIDE_FORWARD_OFFSET * size_scale
        + Vec3::Y * (CHICK_FRESH_EGG_RIDE_VERTICAL_OFFSET * size_scale + bob)
}

fn orbit_egg_position(
    owner_position: Vec3,
    facing: Vec3,
    size_scale: f32,
    age: ElapsedTicks,
) -> Vec3 {
    let facing = normalized_or_forward(facing);
    let side = chick_skill_side_vec(facing);
    let (cos, sin) = crate::canonical_math::chick_orbit_basis(age.get());
    let orbit = (facing * cos + side * sin)
        * CHICK_ORBIT_EGG_ORBIT_RADIUS
        * chick_skill_size_scale(size_scale);
    owner_position + orbit + Vec3::Y * CHICK_ORBIT_EGG_HEIGHT * chick_skill_size_scale(size_scale)
}

fn orbit_egg_rotation(facing: Vec3, age: f32) -> Quat {
    let angle = age * CHICK_ORBIT_EGG_ANGULAR_SPEED;
    projectile_rotation(facing) * Quat::from_rotation_y(angle) * Quat::from_rotation_x(angle * 0.35)
}

fn chick_skill_impact_profile(
    skill: &ActiveChickSkill,
    feel: &CombatFeelTuning,
) -> crate::combat::ImpactProfile {
    let payload_id = skill
        .payload_id
        .expect("visual-only Chick skills should not build impact profiles");
    let mut profile = impact_profile_from_payload_with_feel(
        skill.owner.index(),
        skill.source,
        payload_id,
        1.0,
        1.0,
        1.0,
        skill.guard_stamina_damage,
        feel,
    );
    profile.shape_id = Some(skill.shape_id);
    profile.attacker_style = Some(skill.owner_style);
    profile
}

fn chick_skill_overlaps_target(
    skill: &ActiveChickSkill,
    origin: Vec3,
    target_transform: &SimPosition,
) -> bool {
    match skill.kind {
        ChickSkillKind::FreshEggRide => false,
        ChickSkillKind::SunnySplash => {
            let target_position = target_transform.translation;
            let rendered_radius =
                skill.radius * sunny_splash_canonical_scale(skill.age) + FIGHTER_RADIUS;
            debug_assert!(rendered_radius >= 0.0);
            flat_distance_squared(origin, target_position) <= rendered_radius * rendered_radius
                && (target_position.y - origin.y).abs()
                    <= CHICK_SUNNY_SPLASH_VERTICAL_REACH * skill.size_scale
        }
        ChickSkillKind::OmeletField => {
            let target_position = target_transform.translation;
            let rendered_radius =
                skill.radius * omelet_field_canonical_scale(skill.age) + FIGHTER_RADIUS;
            debug_assert!(rendered_radius >= 0.0);
            flat_distance_squared(origin, target_position) <= rendered_radius * rendered_radius
                && (target_position.y - origin.y).abs()
                    <= CHICK_OMELET_FIELD_VERTICAL_REACH * skill.size_scale
        }
        _ => {
            let target = target_transform.translation + Vec3::Y * (FIGHTER_HEIGHT * 0.58);
            let combined_radius = skill.radius + FIGHTER_RADIUS;
            debug_assert!(combined_radius >= 0.0);
            canonical_math::vec3_distance_squared(target, origin)
                <= combined_radius * combined_radius
        }
    }
}

pub fn chick_skill_lock_target(
    owner: FighterId,
    origin: Vec3,
    facing: Vec3,
    aim_held: bool,
    state: &MatchState,
    targets: &[BeeSkillTargetSnapshot],
) -> Option<FighterId> {
    if !aim_held {
        return None;
    }
    let facing = normalized_or_forward(facing);
    targets
        .iter()
        .filter(|target| target.fighter_id != owner)
        .filter(|target| {
            state.combat_target_allowed_for_state(owner.index(), target.fighter_id.index())
        })
        .filter_map(|target| {
            let offset = Vec3::new(
                target.position.x - origin.x,
                0.0,
                target.position.z - origin.z,
            );
            let distance_squared = canonical_math::vec3_length_squared(offset);
            if distance_squared > CHICK_SKILL_LOCK_RANGE * CHICK_SKILL_LOCK_RANGE
                || distance_squared <= 0.01 * 0.01
            {
                return None;
            }
            let direction = canonical_math::vec3_normalize_or_zero(offset);
            (direction.dot(facing) >= CHICK_SKILL_LOCK_CONE_DOT)
                .then_some((target.fighter_id, distance_squared))
        })
        .min_by(|(fighter_a, distance_a), (fighter_b, distance_b)| {
            distance_a
                .total_cmp(distance_b)
                .then_with(|| fighter_a.cmp(fighter_b))
        })
        .map(|(entity, _)| entity)
}

fn chick_skill_can_hit_target(
    skill: &ActiveChickSkill,
    target: FighterId,
    state: &MatchState,
) -> bool {
    if skill.kind == ChickSkillKind::FreshEggRide || skill.payload_id.is_none() {
        return false;
    }
    target != skill.owner
        && state.combat_target_allowed_for_state(skill.owner.index(), target.index())
}

fn chick_skill_consumed_on_hit(kind: ChickSkillKind) -> bool {
    matches!(
        kind,
        ChickSkillKind::ShellChip
            | ChickSkillKind::FriedEggDisc
            | ChickSkillKind::EggCupMortar
            | ChickSkillKind::FreshEggDrop
            | ChickSkillKind::EggplantRoll
    )
}

fn chick_skill_uses_hit_memory(kind: ChickSkillKind) -> bool {
    !matches!(
        kind,
        ChickSkillKind::OrbitEgg | ChickSkillKind::FreshEggRide
    )
}

fn fresh_egg_drop_touched_ground(
    skill: &ActiveChickSkill,
    position: Vec3,
    arena: &ArenaDefinition,
) -> bool {
    if skill.kind != ChickSkillKind::FreshEggDrop || skill.age.as_millis_floor() <= 80 {
        return false;
    }
    let ground = ground_height(arena, position.x, position.z);
    position.y <= ground + 0.08
}

fn chick_projectile_touched_ground(
    skill: &ActiveChickSkill,
    position: Vec3,
    arena: &ArenaDefinition,
) -> bool {
    if skill.kind != ChickSkillKind::EggCupMortar || skill.age.as_millis_floor() <= 180 {
        return false;
    }
    let ground = ground_height(arena, position.x, position.z);
    position.y <= ground + 0.08
}

fn target_position(fighter: FighterId, targets: &[BeeSkillTargetSnapshot]) -> Option<Vec3> {
    targets
        .iter()
        .find(|target| target.fighter_id == fighter)
        .map(|target| target.position)
}

fn chick_omelet_field_center(
    arena: &ArenaDefinition,
    origin: Vec3,
    facing: Vec3,
    size_scale: f32,
) -> Vec3 {
    grounded_position(
        arena,
        origin + normalized_or_forward(facing) * CHICK_OMELET_FIELD_CENTER_OFFSET * size_scale,
        0.04,
    )
}

fn grounded_position(arena: &ArenaDefinition, position: Vec3, clearance: f32) -> Vec3 {
    let ground = ground_height(arena, position.x, position.z);
    Vec3::new(position.x, ground + clearance, position.z)
}

fn should_despawn_skill(position: Vec3, arena: &ArenaDefinition) -> bool {
    debug_assert!(arena.ringout_radius >= 0.0);
    position.y < arena.ringout_y
        || canonical_math::vec2_length_squared(Vec2::new(position.x, position.z))
            > arena.ringout_radius * arena.ringout_radius
}

fn ground_height(arena: &ArenaDefinition, x: f32, z: f32) -> f32 {
    ground_support_for_arena_with_radius(arena, x, z, 0.0)
        .height()
        .unwrap_or(ARENA_TOP_Y)
}

fn impact_package(kind: ChickSkillKind) -> FeedbackPackageId {
    match kind {
        ChickSkillKind::SunnySplash | ChickSkillKind::OmeletField => {
            FeedbackPackageId::SpecialHazardImpact
        }
        _ => FeedbackPackageId::SpecialProjectileImpact,
    }
}

fn despawn_package(kind: ChickSkillKind) -> FeedbackPackageId {
    match kind {
        ChickSkillKind::SunnySplash | ChickSkillKind::OmeletField => {
            FeedbackPackageId::SpecialHazardFade
        }
        _ => FeedbackPackageId::SpecialProjectileRecover,
    }
}

fn chick_skill_side_vec(facing: Vec3) -> Vec3 {
    canonical_math::vec3_normalize_or_zero(Vec3::new(-facing.z, 0.0, facing.x))
}

fn flat_direction(origin: Vec3, target: Vec3) -> Vec3 {
    canonical_math::vec3_normalize_or_zero(Vec3::new(target.x - origin.x, 0.0, target.z - origin.z))
}

fn flat_distance_squared(a: Vec3, b: Vec3) -> f32 {
    canonical_math::vec2_distance_squared(Vec2::new(a.x, a.z), Vec2::new(b.x, b.z))
}

fn normalized_or_forward(direction: Vec3) -> Vec3 {
    if canonical_math::vec3_length_squared(direction) > 0.01 {
        canonical_math::vec3_normalize_or(direction, Vec3::Z)
    } else {
        Vec3::Z
    }
}

fn projectile_rotation(facing: Vec3) -> Quat {
    let facing = normalized_or_forward(facing);
    Quat::from_rotation_arc(Vec3::Z, facing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::characters::{CharacterKind, CharacterMoveCatalog, FighterCharacter};
    use crate::combat::{begin_contact_collection, resolve_contacts};
    use crate::components::{FighterAction, FighterGrabState, FighterUltimateState};
    use crate::equipment::{EquipmentKind, FighterEquipment};
    use crate::game_state::MatchTelemetry;
    use crate::reactions::ReactionFamilyId;
    use crate::sim_event::{PresentationEventCursor, PresentationEventRouter, SimEventJournal};
    use crate::styles::FighterStyle;
    use crate::techniques::attack_payload_definition;

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct FrozenChickTargetState {
        fighter: FighterId,
        health_bits: u32,
        stamina_bits: u32,
        last_attacker: Option<FighterId>,
        action: FighterAction,
        reaction: Option<ReactionFamilyId>,
        velocity_bits: [u32; 3],
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct FrozenChickContactFixture {
        accepted_targets: Vec<FighterId>,
        events: Vec<SimEvent>,
        source: SimEntityId,
        source_hit_memory: u8,
        source_age_ticks: u32,
        source_lifetime_ticks: u32,
        source_repeat_ticks: Option<u32>,
        target_state: Vec<FrozenChickTargetState>,
    }

    fn fighter(index: u8) -> FighterId {
        FighterId::new(index).expect("test fighter index should be valid")
    }

    fn sim(index: u32) -> SimEntityId {
        SimEntityId::new(SimEntityKind::ChickSkill, index, 1)
    }

    fn arena() -> &'static ArenaDefinition {
        crate::arena_defs::arena_definition(0)
    }

    fn local_entity(index: u32) -> Entity {
        Entity::from_raw_u32(index).expect("test entity index should be valid")
    }

    fn assert_vec3_close(actual: Vec3, expected: Vec3) {
        assert!(
            (actual - expected).length() <= 0.0001,
            "expected {actual:?} to be close to {expected:?}"
        );
    }

    fn contact_fixture_state() -> MatchState {
        let mut state = MatchState::default();
        state.rules = crate::game_state::RULE_PRESETS[1];
        state.rule_index = 1;
        state.set_active_slots([true, true, true, false]);
        state.reset_for_new_match();
        state
    }

    fn contact_fixture_fighter(id: FighterId, position: Vec3) -> impl Bundle {
        (
            Fighter {
                id: id.index(),
                name: "Chick contact fixture",
                color: Color::WHITE,
                spawn: position,
            },
            FighterCharacter::new(CharacterKind::Cat),
            FighterStats::default(),
            FighterMotor {
                grounded: true,
                facing: Vec3::Z,
                ..default()
            },
            FighterActionState::default(),
            FighterGrabState::default(),
            FighterUltimateState::default(),
            FighterStyle {
                kind: FighterStyleKind::Anchor,
            },
            FighterEquipment::new(EquipmentKind::CounterCell),
            SimPosition::new(position),
        )
    }

    fn spawn_contact_fixture_chick_skill(app: &mut App, position: Vec3) -> (Entity, SimEntityId) {
        let skill = active_chick_skill(
            ChickSkillKind::SunnySplash,
            FighterId::ZERO,
            0,
            FighterStyleKind::Anchor,
            Vec3::Z,
            Vec3::ZERO,
            1.0,
        );
        let entity = app
            .world_mut()
            .spawn((skill, SimPosition::new(position)))
            .id();
        let stable = app
            .world_mut()
            .resource_mut::<SimulationIdentityAllocator>()
            .try_allocate(SimEntityKind::ChickSkill, entity)
            .unwrap();
        let source = stable.id();
        app.world_mut().entity_mut(entity).insert(stable);
        (entity, source)
    }

    fn run_frozen_chick_contact_fixture(reverse_ecs_allocation: bool) -> FrozenChickContactFixture {
        let owner = FighterId::ZERO;
        let target_a = fighter(1);
        let target_b = fighter(2);
        let target_position = Vec3::new(0.0, ARENA_TOP_Y, 0.0);

        let mut app = App::new();
        app.insert_resource(contact_fixture_state())
            .insert_resource(ActiveArena::default())
            .init_resource::<SimulationIdentityAllocator>()
            .init_resource::<CombatFeelTuning>()
            .init_resource::<CharacterMoveCatalog>()
            .init_resource::<Hitstop>()
            .init_resource::<MatchTelemetry>()
            .init_resource::<ContactBuffer>()
            .insert_resource(TickEventBuffer::new(SimTick(79)))
            .add_systems(
                Update,
                (
                    begin_contact_collection,
                    collect_chick_skill_contacts,
                    resolve_contacts,
                    apply_chick_skill_contact_outcomes,
                )
                    .chain(),
            );

        let early_source = (!reverse_ecs_allocation)
            .then(|| spawn_contact_fixture_chick_skill(&mut app, target_position));
        let fighter_order = if reverse_ecs_allocation {
            [target_b, target_a, owner]
        } else {
            [owner, target_a, target_b]
        };
        for fighter_id in fighter_order {
            let position = if fighter_id == owner {
                target_position + Vec3::X * 5.0
            } else {
                target_position
            };
            app.world_mut()
                .spawn(contact_fixture_fighter(fighter_id, position));
        }
        let (source_entity, source) = early_source
            .unwrap_or_else(|| spawn_contact_fixture_chick_skill(&mut app, target_position));

        app.update();

        let accepted_targets = {
            let contacts = app.world().resource::<ContactBuffer>();
            (0..contacts.len())
                .filter_map(|index| {
                    let record = contacts.record(index)?;
                    let outcome = contacts.outcome(index)?;
                    (record.source.entity() == Some(source)
                        && matches!(
                            outcome.kind,
                            ContactOutcomeKind::Accepted | ContactOutcomeKind::Guarded
                        ))
                    .then_some(record.target)
                })
                .collect()
        };
        let events = app
            .world()
            .resource::<TickEventBuffer>()
            .iter()
            .copied()
            .collect();
        let skill = app
            .world()
            .get::<ActiveChickSkill>(source_entity)
            .expect("Sunny Splash persists after its frozen multi-target batch");
        let (source_hit_memory, source_age_ticks, source_lifetime_ticks, source_repeat_ticks) = (
            skill.already_hit.bits(),
            skill.age.get(),
            skill.lifetime.remaining(),
            skill.repeat_timer.map(TickTimer::remaining),
        );
        let target_state = {
            let world = app.world_mut();
            let mut fighters =
                world.query::<(&Fighter, &FighterStats, &FighterMotor, &FighterActionState)>();
            let mut state = fighters
                .iter(world)
                .filter_map(|(fighter, stats, motor, action)| {
                    let fighter = FighterId::from_index(fighter.id)?;
                    (fighter != owner).then_some(FrozenChickTargetState {
                        fighter,
                        health_bits: stats.health.to_bits(),
                        stamina_bits: stats.stamina.to_bits(),
                        last_attacker: stats.last_attacker,
                        action: action.action,
                        reaction: action.reaction_family,
                        velocity_bits: [
                            motor.velocity.x.to_bits(),
                            motor.velocity.y.to_bits(),
                            motor.velocity.z.to_bits(),
                        ],
                    })
                })
                .collect::<Vec<_>>();
            state.sort_by_key(|target| target.fighter);
            state
        };

        FrozenChickContactFixture {
            accepted_targets,
            events,
            source,
            source_hit_memory,
            source_age_ticks,
            source_lifetime_ticks,
            source_repeat_ticks,
            target_state,
        }
    }

    #[test]
    fn frozen_chick_hazard_is_independent_of_target_and_source_ecs_allocation_order() {
        let forward = run_frozen_chick_contact_fixture(false);
        let reversed = run_frozen_chick_contact_fixture(true);

        assert_eq!(forward, reversed);
        assert_eq!(forward.accepted_targets, vec![fighter(1), fighter(2)]);
        assert_eq!(
            forward.source_hit_memory,
            (1 << fighter(1).index()) | (1 << fighter(2).index())
        );
        assert_eq!(forward.source_age_ticks, 1);
        assert!(forward.source_lifetime_ticks > 0);
        assert!(forward.source_repeat_ticks.is_some_and(|ticks| ticks > 0));
        assert!(
            forward
                .target_state
                .iter()
                .all(|target| target.health_bits != crate::constants::MAX_HEALTH.to_bits()),
            "{forward:?}"
        );
        assert_eq!(
            forward
                .events
                .iter()
                .filter_map(|event| match event.kind {
                    SimEventKind::HitConfirmed { victim, .. } => Some(victim),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            vec![fighter(1), fighter(2)]
        );
        assert_eq!(
            forward
                .events
                .iter()
                .map(|event| event.id)
                .collect::<Vec<_>>(),
            vec![
                SimEventId {
                    tick: SimTick(79),
                    source: SimEventSource::Entity(forward.source),
                    ordinal: 0,
                },
                SimEventId {
                    tick: SimTick(79),
                    source: SimEventSource::Entity(forward.source),
                    ordinal: 1,
                },
            ]
        );
    }

    fn presentation_intent_at(tick: u64, ordinal: u16) -> ChickPresentationIntent {
        let entity = sim(0);
        ChickPresentationIntent {
            event_id: SimEventId {
                tick: SimTick(tick),
                source: SimEventSource::Entity(entity),
                ordinal,
            },
            entity,
            kind: ChickPresentationKind::Lifecycle {
                event: AbilityLifecycleEvent::Spawned,
                position: Vec3::ZERO,
                direction: Vec3::X,
                package: Some(FeedbackPackageId::SpecialProjectileStartup),
                cue: Some("release_special_projectile"),
                source: ImpactSource::Projectile,
                priority: 24,
                hud_flash: Some((FighterId::ZERO, 0.12)),
            },
        }
    }

    fn commit_presentation_event(
        journal: &mut SimEventJournal,
        intents: &mut ChickPresentationIntentJournal,
        tick: u64,
    ) -> SimEventId {
        let intent = presentation_intent_at(tick, 0);
        let mut buffer = TickEventBuffer::new(SimTick(tick));
        let event_id = buffer
            .emit(
                SimEventSource::Entity(intent.entity),
                SimEventKind::AbilityLifecycle {
                    entity: intent.entity,
                    event: AbilityLifecycleEvent::Spawned,
                },
            )
            .unwrap();
        journal.commit(&buffer);
        intents
            .record(ChickPresentationIntent { event_id, ..intent })
            .unwrap();
        event_id
    }

    fn spawn_test_chick_skills(
        mut commands: Commands,
        mut identities: ResMut<SimulationIdentityAllocator>,
        mut presentation: ChickPresentationEmitter,
        state: Res<MatchState>,
        arena: Res<ActiveArena>,
    ) {
        spawn_chick_skill_with_presentation(
            &mut commands,
            &mut identities,
            &mut presentation,
            &state,
            arena.definition(),
            FighterId::ZERO,
            0,
            FighterStyleKind::Anchor,
            Vec3::ZERO,
            Vec3::X,
            false,
            1.0,
            ChickSkillId::ShellPeck,
            &[],
            &[],
        );
    }

    #[test]
    fn headless_chick_spawn_has_only_canonical_components_and_semantic_events() {
        let mut app = App::new();
        app.init_resource::<SimulationIdentityAllocator>()
            .insert_resource(TickEventBuffer::new(SimTick(6)))
            .insert_resource(MatchState::default())
            .insert_resource(ActiveArena::default())
            .init_resource::<CombatFeelTuning>()
            .init_resource::<Hitstop>()
            .init_resource::<ContactBuffer>()
            .add_systems(
                Update,
                (
                    spawn_test_chick_skills,
                    collect_chick_skill_contacts,
                    apply_chick_skill_contact_outcomes,
                )
                    .chain(),
            );

        app.update();

        let world = app.world_mut();
        let mut skills = world.query_filtered::<Entity, With<ActiveChickSkill>>();
        let entities = skills.iter(world).collect::<Vec<_>>();
        assert_eq!(entities.len(), 2);
        for entity in entities {
            assert!(world.get::<StableSimEntity>(entity).is_some());
            assert!(world.get::<SimPosition>(entity).is_some());
            assert!(world.get::<Transform>(entity).is_none());
            assert!(world.get::<SceneRoot>(entity).is_none());
            assert!(world.get::<ChickSkillVisualRoot>(entity).is_none());
        }
        assert!(world.get_resource::<ChickSkillAssets>().is_none());
        assert!(world.get_resource::<EffectAssets>().is_none());
        assert!(world.get_resource::<HitEffects>().is_none());
        assert_eq!(world.resource::<TickEventBuffer>().len(), 2);
    }

    #[test]
    fn chick_presentation_journal_is_bounded_and_validates_semantics() {
        let mut intents = ChickPresentationIntentJournal::default();
        for tick in 0..SIM_EVENT_HISTORY_TICKS as u64 {
            for ordinal in 0..MAX_SIM_EVENTS_PER_TICK as u16 {
                intents
                    .record(presentation_intent_at(tick, ordinal))
                    .unwrap();
            }
        }
        assert_eq!(intents.len(), intents.capacity());

        let bad = presentation_intent_at(999, MAX_SIM_EVENTS_PER_TICK as u16);
        assert_eq!(
            intents.record(bad),
            Err(EventEmitError::CapacityExceeded {
                capacity: MAX_SIM_EVENTS_PER_TICK,
            })
        );
        assert_eq!(intents.metrics().rejected, 1);

        let intent = presentation_intent_at(7, 0);
        let wrong_semantic = SimEvent {
            id: intent.event_id,
            kind: SimEventKind::AbilityLifecycle {
                entity: intent.entity,
                event: AbilityLifecycleEvent::Despawned,
            },
        };
        assert!(!chick_presentation_matches_event(wrong_semantic, intent));
    }

    #[test]
    fn chick_events_survive_render_stall_and_rollback_exactly_once() {
        let mut journal = SimEventJournal::default();
        let mut intents = ChickPresentationIntentJournal::default();
        for tick in 30..33 {
            commit_presentation_event(&mut journal, &mut intents, tick);
        }

        let mut cursor = PresentationEventCursor::default();
        let mut router = PresentationEventRouter::default();
        let mut presented = Vec::new();
        cursor
            .route_available(&journal, &mut router, Some(SimTick(32)), |event| {
                if let Some(intent) = intents.get(event.id)
                    && chick_presentation_matches_event(event, intent)
                {
                    presented.push(event.id);
                }
            })
            .unwrap();
        assert_eq!(presented.len(), 3);

        let retained = SimTick(30);
        journal.discard_after(retained);
        cursor.discard_after(retained);
        router.discard_after(retained);
        intents.discard_after(retained);
        for tick in 31..33 {
            commit_presentation_event(&mut journal, &mut intents, tick);
        }
        cursor
            .route_available(&journal, &mut router, Some(SimTick(32)), |event| {
                if let Some(intent) = intents.get(event.id)
                    && chick_presentation_matches_event(event, intent)
                {
                    presented.push(event.id);
                }
            })
            .unwrap();

        assert_eq!(presented.len(), 3);
        assert_eq!(router.metrics().duplicate_events_suppressed, 2);
        assert_eq!(intents.metrics().discarded, 2);
    }

    #[test]
    fn chick_asset_paths_exist_for_runtime_loading() {
        assert!(std::path::Path::new("assets/food/kenney_food_kit/egg.glb").exists());
        assert!(std::path::Path::new("assets/food/kenney_food_kit/egg-half.glb").exists());
        assert!(std::path::Path::new("assets/food/kenney_food_kit/egg-cooked.glb").exists());
        assert!(std::path::Path::new("assets/food/kenney_food_kit/egg-cup.glb").exists());
        assert!(std::path::Path::new("assets/food/kenney_food_kit/eggplant.glb").exists());
    }

    #[test]
    fn active_skill_constants_map_to_chick_payloads() {
        let cases = [
            (
                ChickSkillKind::ShellChip,
                Some(AttackPayloadId::ChickShellChip),
                CHICK_SHELL_CHIP_RADIUS,
                CHICK_SHELL_CHIP_LIFETIME,
                None,
                5.0,
            ),
            (
                ChickSkillKind::FriedEggDisc,
                Some(AttackPayloadId::ChickFriedEggDisc),
                CHICK_FRIED_DISC_RADIUS,
                CHICK_FRIED_DISC_LIFETIME,
                None,
                8.0,
            ),
            (
                ChickSkillKind::EggCupMortar,
                Some(AttackPayloadId::ChickEggCupMortar),
                CHICK_EGG_CUP_RADIUS,
                CHICK_EGG_CUP_LIFETIME,
                None,
                11.0,
            ),
            (
                ChickSkillKind::OrbitEgg,
                Some(AttackPayloadId::ChickOrbitEgg),
                CHICK_ORBIT_EGG_RADIUS,
                CHICK_ORBIT_EGG_LIFETIME,
                None,
                2.0,
            ),
            (
                ChickSkillKind::OrbitEggLaunch,
                Some(AttackPayloadId::ChickOrbitEgg),
                CHICK_ORBIT_EGG_LAUNCH_RADIUS,
                CHICK_ORBIT_EGG_LAUNCH_LIFETIME,
                None,
                2.0,
            ),
            (
                ChickSkillKind::OrbitEggReturn,
                Some(AttackPayloadId::ChickOrbitEggLaunch),
                CHICK_ORBIT_EGG_RETURN_RADIUS,
                CHICK_ORBIT_EGG_RETURN_LIFETIME,
                None,
                10.0,
            ),
            (
                ChickSkillKind::FreshEggDrop,
                Some(AttackPayloadId::ChickFreshEggDrop),
                CHICK_FRESH_EGG_RADIUS,
                CHICK_FRESH_EGG_LIFETIME,
                None,
                7.0,
            ),
            (
                ChickSkillKind::FreshEggRide,
                None,
                0.0,
                CHICK_FRESH_EGG_RIDE_LIFETIME,
                None,
                0.0,
            ),
            (
                ChickSkillKind::EggplantRoll,
                Some(AttackPayloadId::ChickEggplantRoll),
                CHICK_EGGPLANT_RADIUS,
                CHICK_EGGPLANT_LIFETIME,
                None,
                12.0,
            ),
            (
                ChickSkillKind::SunnySplash,
                Some(AttackPayloadId::ChickSunnySplash),
                CHICK_SUNNY_SPLASH_RADIUS,
                CHICK_SUNNY_SPLASH_LIFETIME,
                Some(CHICK_SUNNY_SPLASH_TICK),
                5.0,
            ),
            (
                ChickSkillKind::OmeletField,
                Some(AttackPayloadId::ChickOmeletField),
                CHICK_OMELET_FIELD_RADIUS,
                CHICK_OMELET_FIELD_LIFETIME,
                Some(CHICK_OMELET_FIELD_TICK),
                6.0,
            ),
        ];

        for (kind, payload, radius, lifetime, repeat, guard_stamina_damage) in cases {
            let skill = active_chick_skill(
                kind,
                fighter(0),
                0,
                FighterStyleKind::Anchor,
                Vec3::X,
                Vec3::ZERO,
                1.0,
            );

            assert_eq!(skill.payload_id, payload);
            assert_eq!(skill.radius, radius);
            assert_eq!(skill.lifetime, TickTimer::from_seconds_ceil(lifetime));
            assert_eq!(
                skill.repeat_interval,
                repeat.map(TickTimer::from_seconds_ceil)
            );
            assert_eq!(skill.guard_stamina_damage, guard_stamina_damage);
        }
    }

    #[test]
    fn orbit_egg_position_tracks_owner_and_rotates() {
        let mut skill = active_chick_skill(
            ChickSkillKind::OrbitEgg,
            fighter(0),
            0,
            FighterStyleKind::Anchor,
            Vec3::X,
            Vec3::ZERO,
            1.0,
        );
        let mut transform = SimPosition::default();

        update_orbit_egg_motion(&mut skill, &mut transform, Some(Vec3::new(1.0, 0.0, 2.0)));
        assert_vec3_close(
            transform.translation,
            Vec3::new(1.0, 0.0, 2.0)
                + Vec3::X * CHICK_ORBIT_EGG_ORBIT_RADIUS
                + Vec3::Y * CHICK_ORBIT_EGG_HEIGHT,
        );

        skill.age = ElapsedTicks::from_ticks(crate::simulation::seconds_to_ticks_ceil(0.25));
        update_orbit_egg_motion(&mut skill, &mut transform, Some(Vec3::new(3.0, 0.0, -1.0)));
        let expected_offset = orbit_egg_position(
            Vec3::ZERO,
            Vec3::X,
            1.0,
            ElapsedTicks::from_ticks(crate::simulation::seconds_to_ticks_ceil(0.25)),
        );
        assert_vec3_close(
            transform.translation,
            Vec3::new(3.0, 0.0, -1.0) + expected_offset,
        );
    }

    #[test]
    fn orbit_egg_visual_is_five_times_base_egg_size() {
        assert_vec3_close(
            chick_skill_visual_scale(ChickSkillKind::OrbitEgg, 1.0, 0.0),
            Vec3::splat(0.44 * CHICK_ORBIT_EGG_VISUAL_SCALE),
        );
        assert_vec3_close(
            chick_skill_visual_scale(ChickSkillKind::OrbitEggLaunch, 1.0, 0.0),
            Vec3::splat(0.44 * CHICK_ORBIT_EGG_VISUAL_SCALE),
        );
        assert_vec3_close(
            chick_skill_visual_scale(ChickSkillKind::OrbitEggReturn, 1.0, 0.0),
            Vec3::splat(0.44 * CHICK_ORBIT_EGG_VISUAL_SCALE),
        );
        assert_eq!(CHICK_ORBIT_EGG_VISUAL_SCALE, 5.0);
    }

    #[test]
    fn fresh_egg_drop_and_ride_use_triple_jump_x_egg_size() {
        assert_vec3_close(
            chick_skill_visual_scale(ChickSkillKind::FreshEggDrop, 1.0, 0.0),
            Vec3::splat(CHICK_FRESH_EGG_VISUAL_SCALE),
        );
        assert_vec3_close(
            chick_skill_visual_scale(ChickSkillKind::FreshEggRide, 1.0, 0.0),
            Vec3::splat(CHICK_FRESH_EGG_VISUAL_SCALE),
        );
        assert_eq!(CHICK_FRESH_EGG_SIZE_MULTIPLIER, 3.0);
        assert_eq!(
            CHICK_FRESH_EGG_VISUAL_SCALE,
            CHICK_FRESH_EGG_BASE_VISUAL_SCALE * 3.0
        );
        assert_eq!(CHICK_FRESH_EGG_RADIUS, CHICK_FRESH_EGG_BASE_RADIUS * 3.0);
    }

    #[test]
    fn fresh_egg_ride_follows_owner_with_mount_offset() {
        let mut skill = active_chick_skill(
            ChickSkillKind::FreshEggRide,
            fighter(0),
            0,
            FighterStyleKind::Anchor,
            Vec3::X,
            Vec3::ZERO,
            2.0,
        );
        let owner_action = FighterActionState {
            technique_id: Some(TechniqueId::ChickJumpHeavy),
            ..default()
        };
        let owner_transform = SimPosition::new(Vec3::new(3.0, 1.0, -2.0));
        let mut transform = SimPosition::default();

        update_fresh_egg_ride_motion(
            &mut skill,
            &mut transform,
            Some((&owner_action, &owner_transform)),
        );

        assert_vec3_close(
            transform.translation,
            Vec3::new(3.0, 1.0, -2.0)
                + Vec3::X * CHICK_FRESH_EGG_RIDE_FORWARD_OFFSET * 2.0
                + Vec3::Y * CHICK_FRESH_EGG_RIDE_VERTICAL_OFFSET * 2.0,
        );

        skill.age = ElapsedTicks::from_ticks(crate::simulation::seconds_to_ticks_ceil(
            CHICK_FRESH_EGG_RIDE_LIFETIME * 0.25,
        ));
        update_fresh_egg_ride_motion(
            &mut skill,
            &mut transform,
            Some((&owner_action, &owner_transform)),
        );

        assert_vec3_close(
            transform.translation,
            fresh_egg_ride_position(owner_transform.translation, Vec3::X, 2.0, skill.age),
        );
    }

    #[test]
    fn fresh_egg_ride_expires_when_owner_missing_or_exits_jump_x() {
        let owner = fighter(0);
        let owner_transform = SimPosition::new(Vec3::ZERO);
        let owner_idle = FighterActionState::default();
        let mut transform = SimPosition::default();
        let mut missing_owner_skill = active_chick_skill(
            ChickSkillKind::FreshEggRide,
            owner,
            0,
            FighterStyleKind::Anchor,
            Vec3::X,
            Vec3::ZERO,
            1.0,
        );
        let mut exited_action_skill = active_chick_skill(
            ChickSkillKind::FreshEggRide,
            owner,
            0,
            FighterStyleKind::Anchor,
            Vec3::X,
            Vec3::ZERO,
            1.0,
        );

        update_fresh_egg_ride_motion(&mut missing_owner_skill, &mut transform, None);
        update_fresh_egg_ride_motion(
            &mut exited_action_skill,
            &mut transform,
            Some((&owner_idle, &owner_transform)),
        );

        assert_eq!(missing_owner_skill.lifetime, TickTimer::ZERO);
        assert_eq!(exited_action_skill.lifetime, TickTimer::ZERO);
    }

    #[test]
    fn orbit_egg_expires_when_owner_is_missing() {
        let mut skill = active_chick_skill(
            ChickSkillKind::OrbitEgg,
            fighter(0),
            0,
            FighterStyleKind::Anchor,
            Vec3::X,
            Vec3::ZERO,
            1.0,
        );
        let mut transform = SimPosition::default();

        update_orbit_egg_motion(&mut skill, &mut transform, None);

        assert_eq!(skill.lifetime, TickTimer::ZERO);
    }

    #[test]
    fn orbit_egg_recast_replaces_only_same_owner_orbit_egg() {
        let owner = fighter(0);
        let other_owner = fighter(1);
        let same_owner_orbit = sim(20);
        let same_owner_launch = sim(21);
        let same_owner_return = sim(22);
        let same_owner_shell = sim(23);
        let other_owner_orbit = sim(24);
        let other_owner_launch = sim(25);
        let other_owner_return = sim(26);
        let snapshots = [
            ActiveChickSkillSnapshot {
                id: same_owner_orbit,
                owner,
                kind: ChickSkillKind::OrbitEgg,
                position: Vec3::new(1.0, 1.0, 0.0),
            },
            ActiveChickSkillSnapshot {
                id: same_owner_launch,
                owner,
                kind: ChickSkillKind::OrbitEggLaunch,
                position: Vec3::new(1.5, 1.0, 0.0),
            },
            ActiveChickSkillSnapshot {
                id: same_owner_return,
                owner,
                kind: ChickSkillKind::OrbitEggReturn,
                position: Vec3::new(1.75, 1.0, 0.0),
            },
            ActiveChickSkillSnapshot {
                id: same_owner_shell,
                owner,
                kind: ChickSkillKind::ShellChip,
                position: Vec3::new(2.0, 1.0, 0.0),
            },
            ActiveChickSkillSnapshot {
                id: other_owner_orbit,
                owner: other_owner,
                kind: ChickSkillKind::OrbitEgg,
                position: Vec3::new(3.0, 1.0, 0.0),
            },
            ActiveChickSkillSnapshot {
                id: other_owner_launch,
                owner: other_owner,
                kind: ChickSkillKind::OrbitEggLaunch,
                position: Vec3::new(4.0, 1.0, 0.0),
            },
            ActiveChickSkillSnapshot {
                id: other_owner_return,
                owner: other_owner,
                kind: ChickSkillKind::OrbitEggReturn,
                position: Vec3::new(5.0, 1.0, 0.0),
            },
        ];

        let replacements = owner_orbit_egg_replacements(owner, &snapshots).collect::<Vec<_>>();

        assert_eq!(
            replacements,
            vec![same_owner_orbit, same_owner_launch, same_owner_return]
        );
    }

    #[test]
    fn orbit_egg_launch_uses_same_owner_orbit_position() {
        let owner = fighter(0);
        let other_owner = fighter(1);
        let owner_orbit_position = Vec3::new(2.5, 1.2, -0.75);
        let snapshots = [
            ActiveChickSkillSnapshot {
                id: sim(20),
                owner: other_owner,
                kind: ChickSkillKind::OrbitEgg,
                position: Vec3::new(-4.0, 1.0, 0.0),
            },
            ActiveChickSkillSnapshot {
                id: sim(21),
                owner,
                kind: ChickSkillKind::ShellChip,
                position: Vec3::new(0.0, 1.0, 0.0),
            },
            ActiveChickSkillSnapshot {
                id: sim(22),
                owner,
                kind: ChickSkillKind::OrbitEgg,
                position: owner_orbit_position,
            },
        ];

        let launch = owner_orbit_egg_for_launch(owner, &snapshots).unwrap();

        assert_eq!(launch.id, sim(22));
        assert_eq!(launch.position, owner_orbit_position);
        assert!(owner_orbit_egg_for_launch(fighter(2), &snapshots).is_none());
    }

    #[test]
    fn orbit_egg_recall_uses_all_same_owner_launch_positions() {
        let owner = fighter(0);
        let other_owner = fighter(1);
        let first_owner_launch_position = Vec3::new(4.25, 1.1, -1.5);
        let second_owner_launch_position = Vec3::new(-1.25, 1.1, 2.0);
        let snapshots = [
            ActiveChickSkillSnapshot {
                id: sim(20),
                owner: other_owner,
                kind: ChickSkillKind::OrbitEggLaunch,
                position: Vec3::new(-4.0, 1.0, 0.0),
            },
            ActiveChickSkillSnapshot {
                id: sim(21),
                owner,
                kind: ChickSkillKind::OrbitEggReturn,
                position: Vec3::new(1.0, 1.0, 0.0),
            },
            ActiveChickSkillSnapshot {
                id: sim(22),
                owner,
                kind: ChickSkillKind::OrbitEggLaunch,
                position: first_owner_launch_position,
            },
            ActiveChickSkillSnapshot {
                id: sim(23),
                owner,
                kind: ChickSkillKind::OrbitEggLaunch,
                position: second_owner_launch_position,
            },
        ];

        let recall = owner_launched_orbit_eggs_for_recall(owner, &snapshots).collect::<Vec<_>>();

        assert_eq!(recall.len(), 2);
        assert_eq!(recall[0].id, sim(22));
        assert_eq!(recall[0].position, first_owner_launch_position);
        assert_eq!(recall[1].id, sim(23));
        assert_eq!(recall[1].position, second_owner_launch_position);
        assert_eq!(
            owner_launched_orbit_eggs_for_recall(fighter(2), &snapshots).count(),
            0
        );
    }

    #[test]
    fn ultimate_egg_burst_uses_sixteen_even_radial_directions() {
        let directions = ultimate_egg_burst_directions(Vec3::Z);

        assert_eq!(directions.len(), CHICK_ULTIMATE_EGG_COUNT);
        assert_eq!(CHICK_ULTIMATE_EGG_COUNT, 16);
        assert_eq!(ultimate_egg_burst_directions(Vec3::ZERO), directions);
        assert_vec3_close(directions[0], Vec3::Z);
        assert_vec3_close(directions[8], -Vec3::Z);
        assert_vec3_close(directions[4], -Vec3::X);
        let adjacent_dot = directions[0].dot(directions[1]);
        assert!((adjacent_dot - f32::from_bits(0x3f6c_835e)).abs() < 0.001);

        let x_facing = ultimate_egg_burst_directions(Vec3::X);
        assert_vec3_close(x_facing[0], Vec3::X);
        assert_vec3_close(x_facing[4], Vec3::Z);
        assert_vec3_close(x_facing[8], -Vec3::X);
        assert_vec3_close(x_facing[12], -Vec3::Z);
    }

    #[test]
    fn ultimate_orbit_eggs_use_four_second_launched_egg_control_profile() {
        let skill = ultimate_orbit_egg_skill(fighter(0), 0, FighterStyleKind::Anchor, Vec3::X, 1.0);

        assert_eq!(skill.kind, ChickSkillKind::OrbitEggLaunch);
        assert_eq!(skill.payload_id, Some(AttackPayloadId::ChickOrbitEgg));
        assert_eq!(
            skill.lifetime,
            TickTimer::from_seconds_ceil(CHICK_ULTIMATE_EGG_LIFETIME)
        );
        assert_eq!(CHICK_ULTIMATE_EGG_LIFETIME, 4.0);
        assert_eq!(skill.velocity, Vec3::X * CHICK_ORBIT_EGG_LAUNCH_SPEED);
    }

    #[test]
    fn orbit_egg_is_not_consumed_and_does_not_use_hit_memory() {
        let mut skill = active_chick_skill(
            ChickSkillKind::OrbitEgg,
            fighter(0),
            0,
            FighterStyleKind::Anchor,
            Vec3::X,
            Vec3::ZERO,
            1.0,
        );
        skill.already_hit.insert(fighter(1));

        assert!(!chick_skill_consumed_on_hit(skill.kind));
        assert!(!chick_skill_uses_hit_memory(skill.kind));
        assert!(chick_skill_uses_hit_memory(ChickSkillKind::ShellChip));
    }

    #[test]
    fn launched_orbit_egg_uses_soft_payload_and_hit_memory() {
        let skill = active_chick_skill(
            ChickSkillKind::OrbitEggLaunch,
            fighter(0),
            0,
            FighterStyleKind::Anchor,
            Vec3::X,
            Vec3::ZERO,
            1.0,
        );
        let payload = attack_payload_definition(skill.payload_id.unwrap());

        assert_eq!(skill.payload_id, Some(AttackPayloadId::ChickOrbitEgg));
        assert_eq!(
            payload.reaction_family,
            ReactionFamilyId::ShortStandingStagger
        );
        assert_eq!(skill.guard_stamina_damage, 2.0);
        assert!(!chick_skill_consumed_on_hit(skill.kind));
        assert!(chick_skill_uses_hit_memory(skill.kind));
    }

    #[test]
    fn returning_orbit_egg_uses_hard_payload_and_hit_memory() {
        let skill = active_chick_skill(
            ChickSkillKind::OrbitEggReturn,
            fighter(0),
            0,
            FighterStyleKind::Anchor,
            Vec3::X,
            Vec3::ZERO,
            1.0,
        );
        let payload = attack_payload_definition(skill.payload_id.unwrap());

        assert_eq!(skill.payload_id, Some(AttackPayloadId::ChickOrbitEggLaunch));
        assert_eq!(payload.reaction_family, ReactionFamilyId::SlidingKnockdown);
        assert_eq!(skill.guard_stamina_damage, 10.0);
        assert_eq!(
            chick_skill_visual_scale(ChickSkillKind::OrbitEggReturn, 1.0, skill.age.as_seconds(),),
            Vec3::splat(0.44 * CHICK_ORBIT_EGG_VISUAL_SCALE)
        );
        assert!(!chick_skill_consumed_on_hit(skill.kind));
        assert!(chick_skill_uses_hit_memory(skill.kind));
    }

    #[test]
    fn returning_orbit_egg_homes_to_owner_and_resumes_orbit() {
        let owner = fighter(0);
        let owner_position = Vec3::new(2.0, 0.0, -1.0);
        let expected_anchor = orbit_egg_position(owner_position, Vec3::X, 1.0, ElapsedTicks::ZERO);
        let mut skill = active_chick_skill(
            ChickSkillKind::OrbitEggReturn,
            owner,
            0,
            FighterStyleKind::Anchor,
            Vec3::X,
            Vec3::ZERO,
            1.0,
        );
        skill.already_hit.insert(fighter(1));
        let mut transform = SimPosition::new(expected_anchor - Vec3::new(2.0, 0.0, 0.0));

        update_orbit_egg_return_motion(&mut skill, &mut transform, Some(owner_position), 0.05);

        assert_eq!(skill.kind, ChickSkillKind::OrbitEggReturn);
        assert!(transform.translation.x > expected_anchor.x - 2.0);

        update_orbit_egg_return_motion(&mut skill, &mut transform, Some(owner_position), 0.2);

        assert_eq!(skill.kind, ChickSkillKind::OrbitEgg);
        assert_eq!(
            skill.lifetime,
            TickTimer::from_seconds_ceil(CHICK_ORBIT_EGG_LIFETIME)
        );
        assert_eq!(skill.payload_id, Some(AttackPayloadId::ChickOrbitEgg));
        assert!(skill.already_hit.is_empty());
        assert_vec3_close(transform.translation, expected_anchor);
    }

    #[test]
    fn fresh_egg_drop_cracks_only_after_reaching_ground() {
        let mut skill = active_chick_skill(
            ChickSkillKind::FreshEggDrop,
            fighter(0),
            0,
            FighterStyleKind::Anchor,
            Vec3::X,
            Vec3::ZERO,
            1.0,
        );
        skill.age = ElapsedTicks::from_ticks(crate::simulation::seconds_to_ticks_ceil(0.2));

        assert!(!fresh_egg_drop_touched_ground(
            &skill,
            Vec3::new(2.0, ARENA_TOP_Y + 0.5, 0.0),
            arena(),
        ));
        assert!(fresh_egg_drop_touched_ground(
            &skill,
            Vec3::new(2.0, ARENA_TOP_Y + 0.04, 0.0),
            arena(),
        ));
    }

    #[test]
    fn fresh_egg_ride_is_visual_only_and_fresh_egg_drop_still_attacks() {
        let state = MatchState::default();
        let owner = fighter(0);
        let target = fighter(1);
        let mut drop = active_chick_skill(
            ChickSkillKind::FreshEggDrop,
            owner,
            0,
            FighterStyleKind::Anchor,
            Vec3::X,
            Vec3::ZERO,
            1.0,
        );
        let mut ride = active_chick_skill(
            ChickSkillKind::FreshEggRide,
            owner,
            0,
            FighterStyleKind::Anchor,
            Vec3::X,
            Vec3::ZERO,
            1.0,
        );
        drop.age = ElapsedTicks::from_ticks(crate::simulation::seconds_to_ticks_ceil(0.2));
        ride.age = ElapsedTicks::from_ticks(crate::simulation::seconds_to_ticks_ceil(0.2));

        assert_eq!(drop.payload_id, Some(AttackPayloadId::ChickFreshEggDrop));
        assert_eq!(ride.payload_id, None);
        assert!(chick_skill_can_hit_target(&drop, target, &state));
        assert!(!chick_skill_can_hit_target(&ride, target, &state));
        assert!(chick_skill_consumed_on_hit(drop.kind));
        assert!(!chick_skill_consumed_on_hit(ride.kind));
        assert!(chick_skill_uses_hit_memory(drop.kind));
        assert!(!chick_skill_uses_hit_memory(ride.kind));
        assert!(fresh_egg_drop_touched_ground(
            &drop,
            Vec3::new(0.0, ARENA_TOP_Y + 0.04, 0.0),
            arena(),
        ));
        assert!(!fresh_egg_drop_touched_ground(
            &ride,
            Vec3::new(0.0, ARENA_TOP_Y + 0.04, 0.0),
            arena(),
        ));
        assert!(!chick_projectile_touched_ground(
            &ride,
            Vec3::new(0.0, ARENA_TOP_Y + 0.04, 0.0),
            arena(),
        ));
        assert!(!chick_skill_overlaps_target(
            &ride,
            Vec3::ZERO,
            &SimPosition::new(Vec3::ZERO)
        ));
    }

    #[test]
    fn hazard_repeat_window_clears_contact_memory() {
        let mut skill = active_chick_skill(
            ChickSkillKind::OmeletField,
            fighter(0),
            0,
            FighterStyleKind::Anchor,
            Vec3::X,
            Vec3::ZERO,
            1.0,
        );
        skill.already_hit.insert(fighter(1));
        skill.repeat_timer = Some(TickTimer::from_ticks(1));

        assert!(update_skill_repeat_window(&mut skill));

        assert!(skill.already_hit.is_empty());
        assert_eq!(skill.repeat_timer, skill.repeat_interval);
    }

    #[test]
    fn chick_skill_lock_target_uses_aimed_front_enemy() {
        let mut state = MatchState::default();
        state.active_slots = [true, true, true, false];
        state.active_fighter_count = 3;
        let targets = [
            BeeSkillTargetSnapshot {
                fighter_id: fighter(1),
                position: Vec3::new(3.0, 0.0, 0.0),
            },
            BeeSkillTargetSnapshot {
                fighter_id: fighter(2),
                position: Vec3::new(-2.0, 0.0, 0.0),
            },
        ];

        assert_eq!(
            chick_skill_lock_target(fighter(0), Vec3::ZERO, Vec3::X, true, &state, &targets),
            Some(fighter(1))
        );
        assert_eq!(
            chick_skill_lock_target(fighter(0), Vec3::ZERO, Vec3::X, false, &state, &targets),
            None
        );
    }

    #[test]
    fn chick_skill_lock_target_breaks_equal_distance_ties_by_fighter_id() {
        let mut state = MatchState::default();
        state.rules = crate::game_state::RULE_PRESETS[1];
        state.rule_index = 1;
        state.active_slots = [true, true, true, false];
        state.active_fighter_count = 3;
        let targets = [
            BeeSkillTargetSnapshot {
                fighter_id: fighter(2),
                position: Vec3::new(3.0, 0.0, 1.0),
            },
            BeeSkillTargetSnapshot {
                fighter_id: fighter(1),
                position: Vec3::new(3.0, 0.0, -1.0),
            },
        ];

        assert_eq!(
            chick_skill_lock_target(fighter(0), Vec3::ZERO, Vec3::X, true, &state, &targets),
            Some(fighter(1))
        );
    }

    #[test]
    fn mushroom_size_scale_enlarges_chick_collision_and_visuals() {
        let size_scale = crate::constants::ITEM_GIANT_SIZE_MULTIPLIER;
        let skill = active_chick_skill(
            ChickSkillKind::ShellChip,
            fighter(0),
            0,
            FighterStyleKind::Anchor,
            Vec3::X,
            Vec3::ZERO,
            size_scale,
        );

        assert_eq!(skill.size_scale, size_scale);
        assert_eq!(skill.radius, CHICK_SHELL_CHIP_RADIUS * size_scale);
        assert_vec3_close(
            chick_skill_visual_scale(ChickSkillKind::ShellChip, size_scale, 0.0),
            Vec3::splat(0.38 * size_scale),
        );
        assert_vec3_close(
            chick_skill_visual_scale(ChickSkillKind::OmeletField, size_scale, 0.25),
            Vec3::splat(omelet_field_visual_pulse(0.25) * size_scale),
        );
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn c1_frozen_chick_phases_match_every_v3_reference_tick() {
        let sunny_ticks = crate::simulation::seconds_to_ticks_ceil(CHICK_SUNNY_SPLASH_LIFETIME);
        let omelet_ticks = crate::simulation::seconds_to_ticks_ceil(CHICK_OMELET_FIELD_LIFETIME);
        let orbit_ticks = crate::simulation::seconds_to_ticks_ceil(CHICK_ORBIT_EGG_LIFETIME);
        let fresh_ticks = crate::simulation::seconds_to_ticks_ceil(CHICK_FRESH_EGG_RIDE_LIFETIME);
        assert_eq!(
            (sunny_ticks, omelet_ticks, orbit_ticks, fresh_ticks),
            (69, 123, 480, 34)
        );

        for tick in 0..=sunny_ticks {
            let age = ElapsedTicks::from_ticks(tick);
            assert_eq!(
                sunny_splash_canonical_scale(age).to_bits(),
                sunny_splash_visual_pulse(age.as_seconds()).to_bits(),
                "Sunny Splash reference mismatch at tick {tick}"
            );
        }
        for tick in 0..=omelet_ticks {
            let age = ElapsedTicks::from_ticks(tick);
            assert_eq!(
                omelet_field_canonical_scale(age).to_bits(),
                omelet_field_visual_pulse(age.as_seconds()).to_bits(),
                "Omelet Field reference mismatch at tick {tick}"
            );
        }
        for tick in 0..=orbit_ticks {
            let age = ElapsedTicks::from_ticks(tick).as_seconds();
            let angle = age * CHICK_ORBIT_EGG_ANGULAR_SPEED;
            let (cos, sin) = crate::canonical_math::chick_orbit_basis(tick);
            assert_eq!(
                (cos.to_bits(), sin.to_bits()),
                (angle.cos().to_bits(), angle.sin().to_bits()),
                "Orbit Egg reference mismatch at tick {tick}"
            );
        }
        for tick in 0..=fresh_ticks {
            let age = ElapsedTicks::from_ticks(tick).as_seconds();
            let reference =
                (age / CHICK_FRESH_EGG_RIDE_LIFETIME * TAU).sin() * CHICK_FRESH_EGG_RIDE_BOB_HEIGHT;
            assert_eq!(
                crate::canonical_math::chick_fresh_ride_bob(tick).to_bits(),
                reference.to_bits(),
                "Fresh Egg Ride reference mismatch at tick {tick}"
            );
        }
    }

    #[test]
    fn chick_identity_pool_rejects_overflow_without_evicting_live_skill() {
        let mut capacities = [0; SimEntityKind::ALL.len()];
        capacities[SimEntityKind::ChickSkill.code() as usize] = 1;
        let mut identities = SimulationIdentityAllocator::with_capacities(capacities);
        let live = identities
            .try_allocate(SimEntityKind::ChickSkill, local_entity(1))
            .unwrap();

        assert!(
            identities
                .try_allocate(SimEntityKind::ChickSkill, local_entity(2))
                .is_err()
        );
        assert_eq!(live.id().kind(), SimEntityKind::ChickSkill);
        assert_eq!(identities.mapped_entity(live.id()), Some(local_entity(1)));
        assert_eq!(identities.rejected_spawns(SimEntityKind::ChickSkill), 1);
    }
}
