use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use crate::arena::ground_support_for_arena_with_radius;
use crate::arena_defs::{ActiveArena, ArenaDefinition};
use crate::canonical_math;
use crate::combat::{
    HitEffects, ImpactSource, can_receive_impact, impact_profile_from_payload_with_feel,
};
use crate::components::{Fighter, FighterActionState, FighterMotor, FighterStats, SimPosition};
use crate::constants::{ARENA_TOP_Y, FIGHTER_HEIGHT, FIGHTER_RADIUS, KENNEY_CUBE_PET_SCALE};
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
use crate::techniques::{AttackPayloadId, AttackShapeId, BeeSkillId};

const BEE_SKILL_LOCK_RANGE: f32 = 8.0;
const BEE_SKILL_LOCK_CONE_DOT: f32 = 0.70710677;
const BEE_WORKER_SPEED: f32 = 8.4;
const BEE_WORKER_LIFETIME: f32 = 0.78;
const BEE_WORKER_RADIUS: f32 = 0.3;
const BEE_HONEY_GLOB_SPEED: f32 = 7.8;
const BEE_HONEY_GLOB_LIFT: f32 = 1.35;
const BEE_HONEY_GLOB_GRAVITY: f32 = 9.5;
const BEE_HONEY_GLOB_LIFETIME: f32 = 1.15;
const BEE_HONEY_GLOB_RADIUS: f32 = 0.42;
const BEE_HONEY_PUDDLE_LIFETIME: f32 = 2.4;
const BEE_HONEY_PUDDLE_RADIUS: f32 = 0.68;
const BEE_HONEY_PUDDLE_TICK: f32 = 0.45;
const BEE_HONEY_PUDDLE_DAMPING: f32 = 0.45;
const BEE_HONEY_PUDDLE_VERTICAL_REACH: f32 = 0.38;
const BEE_HOMING_SPEED: f32 = 11.2;
const BEE_HOMING_TURN_RATE: f32 = 12.0;
const BEE_HOMING_LIFETIME: f32 = 1.05;
const BEE_HOMING_RADIUS: f32 = 0.34;
const BEE_ULTIMATE_SWARM_LIFETIME: f32 = 2.4;
const BEE_ULTIMATE_SWARM_RADIUS: f32 = 2.0;
const BEE_ULTIMATE_SWARM_TICK: f32 = 0.3;
const BEE_ULTIMATE_SWARM_VERTICAL_REACH: f32 = 1.7;
const BEE_ULTIMATE_SWARM_GUARD_DAMAGE: f32 = 5.0;
const BEE_ULTIMATE_SWARM_CENTER_OFFSET: f32 = 2.4;
const BEE_ULTIMATE_SWARM_BEE_COUNT: usize = 5;
const BEE_ULTIMATE_SWARM_ORBIT_RADIUS: f32 = 1.35;
const BEE_ULTIMATE_SWARM_ORBIT_SPEED: f32 = 14.0;
const BEE_ULTIMATE_SWARM_BOB_HEIGHT: f32 = 0.28;
const BEE_ULTIMATE_SWARM_BOB_SPEED: f32 = 22.0;
const BEE_ULTIMATE_SWARM_BEE_HEIGHT: f32 = 1.04;
const BEE_ULTIMATE_SWARM_BEE_SCALE: f32 = 0.22;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BeeSkillKind {
    WorkerBee,
    HoneyGlob,
    HoneyPuddle,
    HomingSting,
    UltimateSwarm,
}

#[derive(Clone, Copy, Debug)]
pub struct BeeSkillTargetSnapshot {
    pub fighter_id: FighterId,
    pub position: Vec3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BeeSkillSpawnMode {
    Standard,
    AreaSwarm,
}

#[derive(Component)]
pub struct ActiveBeeSkill {
    pub kind: BeeSkillKind,
    pub owner: FighterId,
    pub owner_style: FighterStyleKind,
    pub payload_id: AttackPayloadId,
    pub shape_id: AttackShapeId,
    pub source: ImpactSource,
    pub facing: Vec3,
    pub velocity: Vec3,
    pub target: Option<FighterId>,
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
pub(crate) enum BeePresentationKind {
    Lifecycle {
        event: AbilityLifecycleEvent,
        position: Vec3,
        direction: Vec3,
        package: Option<FeedbackPackageId>,
        cue: Option<&'static str>,
        source: ImpactSource,
        priority: u8,
    },
    Impact {
        victim: FighterId,
        position: Vec3,
        direction: Vec3,
        package: FeedbackPackageId,
        hazard_cue: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct BeePresentationIntent {
    pub event_id: SimEventId,
    pub entity: SimEntityId,
    pub kind: BeePresentationKind,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct BeePresentationIntentSlot {
    tick: SimTick,
    len: u16,
    occupied: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BeePresentationIntentMetrics {
    pub recorded: u64,
    pub replaced: u64,
    pub rejected: u64,
    pub discarded: u64,
}

#[derive(Resource, Clone, Debug)]
pub struct BeePresentationIntentJournal {
    slots: [BeePresentationIntentSlot; SIM_EVENT_HISTORY_TICKS],
    intents: Box<[Option<BeePresentationIntent>]>,
    len: usize,
    metrics: BeePresentationIntentMetrics,
}

impl Default for BeePresentationIntentJournal {
    fn default() -> Self {
        Self {
            slots: [BeePresentationIntentSlot::default(); SIM_EVENT_HISTORY_TICKS],
            intents: vec![None; SIM_EVENT_HISTORY_TICKS * MAX_SIM_EVENTS_PER_TICK]
                .into_boxed_slice(),
            len: 0,
            metrics: BeePresentationIntentMetrics::default(),
        }
    }
}

impl BeePresentationIntentJournal {
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
    pub const fn metrics(&self) -> BeePresentationIntentMetrics {
        self.metrics
    }

    pub(crate) fn record(&mut self, intent: BeePresentationIntent) -> Result<(), EventEmitError> {
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
            *slot = BeePresentationIntentSlot {
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

    pub(crate) fn get(&self, event_id: SimEventId) -> Option<BeePresentationIntent> {
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
            self.slots[slot_index] = BeePresentationIntentSlot::default();
            self.len = self.len.saturating_sub(usize::from(slot.len));
            self.metrics.discarded = self.metrics.discarded.saturating_add(u64::from(slot.len));
        }
    }
}

impl RollbackEventDiscard for BeePresentationIntentJournal {
    fn discard_after(&mut self, retained_through: SimTick) {
        Self::discard_after(self, retained_through);
    }
}

/// One bounded emission boundary shared by the fixed combat timeline and bee
/// skill updater. Dedicated authorities omit both optional render journals.
#[derive(SystemParam)]
pub(crate) struct BeePresentationEmitter<'w> {
    sim_events: ResMut<'w, TickEventBuffer>,
    bee_intents: Option<ResMut<'w, BeePresentationIntentJournal>>,
}

impl BeePresentationEmitter<'_> {
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
    ) {
        let Ok(event_id) = self.sim_events.emit(
            SimEventSource::Entity(entity),
            SimEventKind::AbilityLifecycle { entity, event },
        ) else {
            return;
        };
        if let Some(intents) = self.bee_intents.as_deref_mut() {
            let _ = intents.record(BeePresentationIntent {
                event_id,
                entity,
                kind: BeePresentationKind::Lifecycle {
                    event,
                    position,
                    direction,
                    package,
                    cue,
                    source,
                    priority,
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
        hazard_cue: bool,
    ) {
        if let Some(intents) = self.bee_intents.as_deref_mut() {
            let _ = intents.record(BeePresentationIntent {
                event_id,
                entity,
                kind: BeePresentationKind::Impact {
                    victim,
                    position,
                    direction,
                    package,
                    hazard_cue,
                },
            });
        }
    }
}

#[derive(Resource, Default)]
pub struct BeeSkillAssets {
    worker_mesh: Handle<Mesh>,
    worker_material: Handle<StandardMaterial>,
    homing_mesh: Handle<Mesh>,
    homing_material: Handle<StandardMaterial>,
    honey_scene: Handle<Scene>,
    honey_puddle_mesh: Handle<Mesh>,
    honey_puddle_material: Handle<StandardMaterial>,
    ultimate_swarm_mesh: Handle<Mesh>,
    ultimate_swarm_material: Handle<StandardMaterial>,
    ultimate_swarm_bee_scene: Handle<Scene>,
}

#[derive(Component, Clone, Copy)]
pub(crate) struct BeeSkillVisualRoot {
    kind: BeeSkillKind,
}

#[derive(Component)]
pub(crate) struct BeeSwarmOrbiter {
    index: usize,
    age: f32,
}

pub fn setup_bee_skill_assets(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.insert_resource(BeeSkillAssets {
        worker_mesh: meshes.add(Sphere::new(0.16).mesh().uv(12, 6)),
        worker_material: materials.add(StandardMaterial {
            base_color: Color::srgb(1.0, 0.78, 0.1),
            emissive: LinearRgba::rgb(0.25, 0.12, 0.0),
            ..default()
        }),
        homing_mesh: meshes.add(Capsule3d::new(0.12, 0.42)),
        homing_material: materials.add(StandardMaterial {
            base_color: Color::srgb(1.0, 0.9, 0.22),
            emissive: LinearRgba::rgb(0.32, 0.2, 0.02),
            ..default()
        }),
        honey_scene: asset_server.load(GltfAssetLabel::Scene(0).from_asset(BEE_HONEY_ASSET)),
        honey_puddle_mesh: meshes.add(Cylinder::new(BEE_HONEY_PUDDLE_RADIUS, 0.045)),
        honey_puddle_material: materials.add(StandardMaterial {
            base_color: Color::srgba(1.0, 0.62, 0.05, 0.78),
            emissive: LinearRgba::rgb(0.22, 0.08, 0.0),
            alpha_mode: AlphaMode::Blend,
            ..default()
        }),
        ultimate_swarm_mesh: meshes.add(Cylinder::new(BEE_ULTIMATE_SWARM_RADIUS, 0.05)),
        ultimate_swarm_material: materials.add(StandardMaterial {
            base_color: Color::srgba(1.0, 0.84, 0.08, 0.28),
            emissive: LinearRgba::rgb(0.26, 0.18, 0.02),
            alpha_mode: AlphaMode::Blend,
            ..default()
        }),
        ultimate_swarm_bee_scene: asset_server
            .load(GltfAssetLabel::Scene(0).from_asset(BEE_SWARM_BEE_ASSET)),
    });
}

pub const BEE_HONEY_ASSET: &str = "food/kenney_food_kit/honey.glb";
pub const BEE_SWARM_BEE_ASSET: &str = "characters/kenney_cube_pets/animal-bee.glb";

pub(crate) fn spawn_bee_skill(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    presentation: &mut BeePresentationEmitter,
    state: &MatchState,
    arena: &ArenaDefinition,
    owner: FighterId,
    owner_id: usize,
    owner_style: FighterStyleKind,
    origin: Vec3,
    facing: Vec3,
    aim_held: bool,
    owner_size_scale: f32,
    spawn_mode: BeeSkillSpawnMode,
    skill: BeeSkillId,
    targets: &[BeeSkillTargetSnapshot],
) {
    let facing = canonical_math::vec3_normalize_or_zero(facing);
    let size_scale = bee_skill_size_scale(owner_size_scale);
    let target =
        bee_skill_target_for_mode(spawn_mode, owner, origin, facing, aim_held, state, targets);
    match skill {
        BeeSkillId::WorkerSwarm => {
            if spawn_mode == BeeSkillSpawnMode::AreaSwarm {
                let side_vec = bee_skill_side_vec(facing);
                for spread in [-0.9, -0.35, 0.35, 0.9] {
                    let spawn = origin
                        + (Vec3::Y * 1.05 + facing * 0.36 + side_vec * spread * 0.34) * size_scale;
                    let direction =
                        canonical_math::vec3_normalize_or_zero(facing + side_vec * spread * 0.55);
                    spawn_worker_bee(
                        commands,
                        identities,
                        presentation,
                        owner,
                        owner_id,
                        owner_style,
                        spawn,
                        direction,
                        None,
                        size_scale,
                    );
                }
            } else {
                for side in [-1.0, 1.0] {
                    let side_vec = bee_skill_side_vec(facing) * side;
                    let spawn =
                        origin + (Vec3::Y * 1.05 + facing * 0.45 + side_vec * 0.28) * size_scale;
                    let direction = target
                        .and_then(|entity| target_position(entity, targets))
                        .map(|position| flat_direction(spawn, position))
                        .filter(|direction| canonical_math::vec3_length_squared(*direction) > 0.01)
                        .unwrap_or_else(|| {
                            canonical_math::vec3_normalize_or_zero(facing + side_vec * 0.18)
                        });
                    spawn_worker_bee(
                        commands,
                        identities,
                        presentation,
                        owner,
                        owner_id,
                        owner_style,
                        spawn,
                        direction,
                        target,
                        size_scale,
                    );
                }
            }
        }
        BeeSkillId::HoneyGlob => {
            if spawn_mode == BeeSkillSpawnMode::AreaSwarm {
                let side_vec = bee_skill_side_vec(facing);
                for spread in [-0.55, 0.55] {
                    let spawn = origin
                        + (Vec3::Y * 1.05 + facing * 0.45 + side_vec * spread * 0.34) * size_scale;
                    let direction =
                        canonical_math::vec3_normalize_or_zero(facing + side_vec * spread * 0.35);
                    spawn_honey_glob(
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
            } else {
                let spawn = origin + (Vec3::Y * 1.05 + facing * 0.5) * size_scale;
                spawn_honey_glob(
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
        BeeSkillId::HomingSting => {
            if spawn_mode == BeeSkillSpawnMode::AreaSwarm {
                let side_vec = bee_skill_side_vec(facing);
                for spread in [-0.6, 0.0, 0.6] {
                    let spawn = origin
                        + (Vec3::Y * 0.98 + facing * 0.58 + side_vec * spread * 0.26) * size_scale;
                    let direction =
                        canonical_math::vec3_normalize_or_zero(facing + side_vec * spread * 0.62);
                    spawn_homing_sting(
                        commands,
                        identities,
                        presentation,
                        owner,
                        owner_id,
                        owner_style,
                        spawn,
                        direction,
                        None,
                        size_scale,
                    );
                }
            } else {
                let spawn = origin + (Vec3::Y * 0.98 + facing * 0.6) * size_scale;
                let direction = target
                    .and_then(|entity| target_position(entity, targets))
                    .map(|position| flat_direction(spawn, position))
                    .filter(|direction| canonical_math::vec3_length_squared(*direction) > 0.01)
                    .unwrap_or(facing);
                spawn_homing_sting(
                    commands,
                    identities,
                    presentation,
                    owner,
                    owner_id,
                    owner_style,
                    spawn,
                    direction,
                    target,
                    size_scale,
                );
            }
        }
        BeeSkillId::UltimateSwarm => {
            spawn_ultimate_swarm(
                commands,
                identities,
                presentation,
                owner,
                owner_id,
                owner_style,
                origin,
                facing,
                size_scale,
                arena,
            );
        }
    }
}

fn spawn_worker_bee(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    presentation: &mut BeePresentationEmitter,
    owner: FighterId,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    direction: Vec3,
    target: Option<FighterId>,
    size_scale: f32,
) {
    let facing = canonical_math::vec3_normalize_or_zero(direction);
    let Some((_, id)) = spawn_canonical_bee_skill(
        commands,
        identities,
        position,
        active_bee_skill(
            BeeSkillKind::WorkerBee,
            owner,
            owner_id,
            owner_style,
            AttackPayloadId::BeeWorkerSting,
            facing,
            facing * BEE_WORKER_SPEED,
            target,
            size_scale,
        ),
    ) else {
        return;
    };
    presentation.emit_lifecycle(
        id,
        AbilityLifecycleEvent::Spawned,
        position,
        facing,
        Some(FeedbackPackageId::SpecialProjectileStartup),
        Some("release_special_projectile"),
        ImpactSource::Projectile,
        24,
    );
}

fn spawn_honey_glob(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    presentation: &mut BeePresentationEmitter,
    owner: FighterId,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    facing: Vec3,
    size_scale: f32,
) {
    let velocity = canonical_math::vec3_normalize_or_zero(facing) * BEE_HONEY_GLOB_SPEED
        + Vec3::Y * BEE_HONEY_GLOB_LIFT;
    let Some((_, id)) = spawn_canonical_bee_skill(
        commands,
        identities,
        position,
        active_bee_skill(
            BeeSkillKind::HoneyGlob,
            owner,
            owner_id,
            owner_style,
            AttackPayloadId::BeeHoneyGlob,
            facing,
            velocity,
            None,
            size_scale,
        ),
    ) else {
        return;
    };
    presentation.emit_lifecycle(
        id,
        AbilityLifecycleEvent::Spawned,
        position,
        facing,
        Some(FeedbackPackageId::SpecialProjectileStartup),
        Some("release_special_projectile"),
        ImpactSource::Projectile,
        24,
    );
}

fn spawn_homing_sting(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    presentation: &mut BeePresentationEmitter,
    owner: FighterId,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    direction: Vec3,
    target: Option<FighterId>,
    size_scale: f32,
) {
    let facing = canonical_math::vec3_normalize_or_zero(direction);
    let Some((_, id)) = spawn_canonical_bee_skill(
        commands,
        identities,
        position,
        active_bee_skill(
            BeeSkillKind::HomingSting,
            owner,
            owner_id,
            owner_style,
            AttackPayloadId::BeeHomingSting,
            facing,
            facing * BEE_HOMING_SPEED,
            target,
            size_scale,
        ),
    ) else {
        return;
    };
    presentation.emit_lifecycle(
        id,
        AbilityLifecycleEvent::Spawned,
        position,
        facing,
        Some(FeedbackPackageId::SpecialProjectileStartup),
        Some("release_special_projectile"),
        ImpactSource::Projectile,
        24,
    );
}

fn spawn_honey_puddle(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    presentation: &mut BeePresentationEmitter,
    owner: FighterId,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    facing: Vec3,
    size_scale: f32,
    arena: &ArenaDefinition,
) {
    let ground = ground_height(arena, position.x, position.z);
    let position = Vec3::new(position.x, ground + 0.035, position.z);
    let Some((_, id)) = spawn_canonical_bee_skill(
        commands,
        identities,
        position,
        active_bee_skill(
            BeeSkillKind::HoneyPuddle,
            owner,
            owner_id,
            owner_style,
            AttackPayloadId::BeeHoneyPuddle,
            facing,
            Vec3::ZERO,
            None,
            size_scale,
        ),
    ) else {
        return;
    };
    presentation.emit_lifecycle(
        id,
        AbilityLifecycleEvent::Spawned,
        position,
        facing,
        Some(FeedbackPackageId::SpecialHazardStartup),
        Some("release_special_projectile"),
        ImpactSource::Hazard,
        24,
    );
}

fn spawn_ultimate_swarm(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    presentation: &mut BeePresentationEmitter,
    owner: FighterId,
    owner_id: usize,
    owner_style: FighterStyleKind,
    origin: Vec3,
    facing: Vec3,
    size_scale: f32,
    arena: &ArenaDefinition,
) {
    let center = bee_ultimate_swarm_center(origin, facing, size_scale, arena);
    let facing = canonical_math::vec3_normalize_or_zero(facing);
    let Some((_, id)) = spawn_canonical_bee_skill(
        commands,
        identities,
        center,
        active_bee_skill(
            BeeSkillKind::UltimateSwarm,
            owner,
            owner_id,
            owner_style,
            AttackPayloadId::BeeUltimateSwarmTick,
            facing,
            Vec3::ZERO,
            None,
            size_scale,
        ),
    ) else {
        return;
    };
    presentation.emit_lifecycle(
        id,
        AbilityLifecycleEvent::Spawned,
        center,
        facing,
        Some(FeedbackPackageId::SpecialHazardStartup),
        Some("release_special_projectile"),
        ImpactSource::Hazard,
        24,
    );
}

fn spawn_canonical_bee_skill(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    position: Vec3,
    skill: ActiveBeeSkill,
) -> Option<(Entity, SimEntityId)> {
    let entity = commands.spawn_empty().id();
    let stable = match identities.try_allocate(SimEntityKind::BeeSkill, entity) {
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

fn active_bee_skill(
    kind: BeeSkillKind,
    owner: FighterId,
    owner_id: usize,
    owner_style: FighterStyleKind,
    payload_id: AttackPayloadId,
    facing: Vec3,
    velocity: Vec3,
    target: Option<FighterId>,
    size_scale: f32,
) -> ActiveBeeSkill {
    debug_assert_eq!(owner.index(), owner_id);
    let size_scale = bee_skill_size_scale(size_scale);
    let (shape_id, source, lifetime, radius, guard_stamina_damage, repeat_interval) = match kind {
        BeeSkillKind::WorkerBee => (
            AttackShapeId::ProjectileBolt,
            ImpactSource::Projectile,
            BEE_WORKER_LIFETIME,
            BEE_WORKER_RADIUS,
            8.0,
            None,
        ),
        BeeSkillKind::HoneyGlob => (
            AttackShapeId::ProjectileBolt,
            ImpactSource::Projectile,
            BEE_HONEY_GLOB_LIFETIME,
            BEE_HONEY_GLOB_RADIUS,
            12.0,
            None,
        ),
        BeeSkillKind::HoneyPuddle => (
            AttackShapeId::HazardField,
            ImpactSource::Hazard,
            BEE_HONEY_PUDDLE_LIFETIME,
            BEE_HONEY_PUDDLE_RADIUS,
            6.0,
            Some(BEE_HONEY_PUDDLE_TICK),
        ),
        BeeSkillKind::HomingSting => (
            AttackShapeId::ProjectileBolt,
            ImpactSource::Projectile,
            BEE_HOMING_LIFETIME,
            BEE_HOMING_RADIUS,
            14.0,
            None,
        ),
        BeeSkillKind::UltimateSwarm => (
            AttackShapeId::HazardField,
            ImpactSource::Hazard,
            BEE_ULTIMATE_SWARM_LIFETIME,
            BEE_ULTIMATE_SWARM_RADIUS,
            BEE_ULTIMATE_SWARM_GUARD_DAMAGE,
            Some(BEE_ULTIMATE_SWARM_TICK),
        ),
    };

    ActiveBeeSkill {
        kind,
        owner,
        owner_style,
        payload_id,
        shape_id,
        source,
        facing: canonical_math::vec3_normalize_or_zero(facing),
        velocity,
        target,
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

fn bee_skill_size_scale(owner_size_scale: f32) -> f32 {
    owner_size_scale.max(0.1)
}

fn bee_skill_visual_scale(kind: BeeSkillKind, size_scale: f32, age: f32) -> Vec3 {
    let size_scale = bee_skill_size_scale(size_scale);
    match kind {
        BeeSkillKind::WorkerBee => Vec3::new(1.25, 0.75, 0.75) * size_scale,
        BeeSkillKind::HoneyGlob => Vec3::splat(0.38 * size_scale),
        BeeSkillKind::HoneyPuddle => Vec3::splat(honey_puddle_visual_pulse(age) * size_scale),
        BeeSkillKind::HomingSting => Vec3::splat(size_scale),
        BeeSkillKind::UltimateSwarm => Vec3::splat(ultimate_swarm_visual_pulse(age) * size_scale),
    }
}

fn honey_puddle_visual_pulse(age: f32) -> f32 {
    0.95 + (age * 8.0).sin().abs() * 0.12
}

fn ultimate_swarm_visual_pulse(age: f32) -> f32 {
    0.96 + (age * 12.0).sin().abs() * 0.08
}

fn honey_puddle_canonical_scale(age: ElapsedTicks) -> f32 {
    crate::canonical_math::bee_honey_puddle_scale(age.get())
}

fn ultimate_swarm_canonical_scale(age: ElapsedTicks) -> f32 {
    crate::canonical_math::bee_ultimate_swarm_scale(age.get())
}

/// Rehydrates renderer components after spawn, rollback restore, or late join.
pub fn attach_missing_bee_skill_visuals(
    mut commands: Commands,
    assets: Res<BeeSkillAssets>,
    skills: Query<
        (Entity, &ActiveBeeSkill, &SimPosition, Option<&Transform>),
        Without<BeeSkillVisualRoot>,
    >,
) {
    for (entity, skill, position, transform) in &skills {
        if transform.is_none() {
            commands
                .entity(entity)
                .insert(Transform::from_translation(position.translation));
        }
        let marker = BeeSkillVisualRoot { kind: skill.kind };
        match skill.kind {
            BeeSkillKind::WorkerBee => {
                commands.entity(entity).insert((
                    Mesh3d(assets.worker_mesh.clone()),
                    MeshMaterial3d(assets.worker_material.clone()),
                    marker,
                    Name::new("Bee worker assist"),
                ));
            }
            BeeSkillKind::HoneyGlob => {
                commands.entity(entity).insert((
                    SceneRoot(assets.honey_scene.clone()),
                    marker,
                    Name::new("Bee honey glob"),
                ));
            }
            BeeSkillKind::HoneyPuddle => {
                commands.entity(entity).insert((
                    Mesh3d(assets.honey_puddle_mesh.clone()),
                    MeshMaterial3d(assets.honey_puddle_material.clone()),
                    marker,
                    Name::new("Bee honey puddle"),
                ));
            }
            BeeSkillKind::HomingSting => {
                commands.entity(entity).insert((
                    Mesh3d(assets.homing_mesh.clone()),
                    MeshMaterial3d(assets.homing_material.clone()),
                    marker,
                    Name::new("Bee homing sting"),
                ));
            }
            BeeSkillKind::UltimateSwarm => {
                commands.entity(entity).insert((
                    Mesh3d(assets.ultimate_swarm_mesh.clone()),
                    MeshMaterial3d(assets.ultimate_swarm_material.clone()),
                    marker,
                    Name::new("Bee ultimate swarm field"),
                ));
                commands.entity(entity).with_children(|parent| {
                    for index in 0..BEE_ULTIMATE_SWARM_BEE_COUNT {
                        parent.spawn((
                            SceneRoot(assets.ultimate_swarm_bee_scene.clone()),
                            bee_swarm_orbiter_transform(index, 0.0),
                            BeeSwarmOrbiter { index, age: 0.0 },
                            Name::new("Bee ultimate mini bee"),
                        ));
                    }
                });
            }
        }
    }
}

/// Derives all non-canonical bee rotations/scales in render Update.
pub fn sync_bee_skill_visuals(
    time: Res<Time>,
    mut skills: Query<(
        &ActiveBeeSkill,
        &BeeSkillVisualRoot,
        &SimPosition,
        &mut Transform,
    )>,
    mut orbiters: Query<
        (&mut BeeSwarmOrbiter, &mut Transform),
        (Without<ActiveBeeSkill>, Without<Fighter>),
    >,
) {
    for (skill, visual, position, mut transform) in &mut skills {
        if visual.kind != skill.kind {
            continue;
        }
        transform.translation = position.translation;
        transform.scale =
            bee_skill_visual_scale(skill.kind, skill.size_scale, skill.age.as_seconds());
        transform.rotation = match skill.kind {
            BeeSkillKind::HoneyGlob => {
                let ticks = skill.age.get() as f32;
                Quat::from_rotation_y(ticks * 0.12) * Quat::from_rotation_x(ticks * 0.08)
            }
            BeeSkillKind::WorkerBee | BeeSkillKind::HomingSting => {
                projectile_rotation(skill.facing)
            }
            BeeSkillKind::HoneyPuddle | BeeSkillKind::UltimateSwarm => Quat::IDENTITY,
        };
    }

    let dt = time.delta_secs();
    for (mut orbiter, mut transform) in &mut orbiters {
        orbiter.age += dt;
        *transform = bee_swarm_orbiter_transform(orbiter.index, orbiter.age);
    }
}

pub fn collect_bee_skill_contacts(
    identities: Res<SimulationIdentityAllocator>,
    state: Res<MatchState>,
    feel: Res<CombatFeelTuning>,
    hitstop: Res<Hitstop>,
    mut contact_buffer: ResMut<ContactBuffer>,
    mut presentation: BeePresentationEmitter,
    mut skills: Query<
        (&StableSimEntity, &mut ActiveBeeSkill, &mut SimPosition),
        (Without<Fighter>, Without<BeeSwarmOrbiter>),
    >,
    mut fighters: ParamSet<(
        Query<(&Fighter, &SimPosition), With<Fighter>>,
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
    for index in 0..identities.capacity(SimEntityKind::BeeSkill) {
        let Some((skill_id, skill_entity)) = identities.entry_at(SimEntityKind::BeeSkill, index)
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
                Some("pulse_special_hazard"),
                skill.source,
                24,
            );
        }
        update_skill_motion(&mut skill, &mut transform, dt, &fighters.p0());

        {
            let target_fighters = fighters.p1();
            for target_id in FighterId::ALL {
                let Some((_target, stats, _motor, action, target_transform)) = target_fighters
                    .iter()
                    .find(|(fighter, ..)| fighter.id == target_id.index())
                else {
                    continue;
                };
                if !bee_skill_can_hit_target(&skill, target_id, &state) {
                    continue;
                }
                if skill.already_hit.contains(target_id)
                    || !can_receive_impact(&stats, &action)
                    || !bee_skill_overlaps_target(&skill, transform.translation, target_transform)
                {
                    continue;
                }

                let profile = bee_skill_impact_profile(&skill, &feel);
                let _ = contact_buffer.push(ContactRecord::new(
                    ContactPhase::Strike,
                    ContactSourceKind::CharacterAbility,
                    skill_id,
                    Some(skill.owner),
                    target_id,
                    skill.payload_id as u16,
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

/// Consumes central outcomes and performs Bee-specific persistence, slowing,
/// child spawning, and lifecycle presentation exactly once per stable source.
pub fn apply_bee_skill_contact_outcomes(
    mut commands: Commands,
    mut identities: ResMut<SimulationIdentityAllocator>,
    active_arena: Res<ActiveArena>,
    mut contact_buffer: ResMut<ContactBuffer>,
    mut presentation: BeePresentationEmitter,
    mut skills: Query<
        (&StableSimEntity, &mut ActiveBeeSkill, &SimPosition),
        (Without<Fighter>, Without<BeeSwarmOrbiter>),
    >,
    mut fighters: Query<(&Fighter, &mut FighterMotor), With<Fighter>>,
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
        if source.kind() != SimEntityKind::BeeSkill {
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

        skill.already_hit.insert(contact.target);
        if bee_skill_consumed_on_hit(skill.kind) {
            skill.lifetime.clear();
        }
        let hazard_cue = if skill.kind == BeeSkillKind::HoneyPuddle {
            fighters
                .iter_mut()
                .find(|(fighter, _)| fighter.id == contact.target.index())
                .is_some_and(|(_, mut motor)| {
                    if !motor.grounded {
                        return false;
                    }
                    motor.velocity.x *= BEE_HONEY_PUDDLE_DAMPING;
                    motor.velocity.z *= BEE_HONEY_PUDDLE_DAMPING;
                    true
                })
        } else {
            false
        };
        if let Some(event_id) = outcome.event_id {
            presentation.record_impact(
                event_id,
                source,
                contact.target,
                contact.contact_point.to_vec3() + Vec3::Y * (FIGHTER_HEIGHT * 0.58),
                skill.facing,
                impact_package(skill.kind),
                hazard_cue,
            );
        }
    }

    for index in 0..identities.capacity(SimEntityKind::BeeSkill) {
        let Some((skill_id, skill_entity)) = identities.entry_at(SimEntityKind::BeeSkill, index)
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

        let glob_grounded =
            honey_glob_touched_ground(&skill, transform.translation, active_arena.definition());
        if !skill.lifetime.active()
            || glob_grounded
            || should_despawn_skill(transform.translation, active_arena.definition())
        {
            if skill.kind == BeeSkillKind::HoneyGlob {
                spawn_honey_puddle(
                    &mut commands,
                    &mut identities,
                    &mut presentation,
                    skill.owner,
                    skill.owner.index(),
                    skill.owner_style,
                    transform.translation,
                    skill.facing,
                    skill.size_scale,
                    active_arena.definition(),
                );
            } else if !hit_this_tick {
                presentation.emit_lifecycle(
                    skill_id,
                    AbilityLifecycleEvent::Despawned,
                    transform.translation,
                    skill.facing,
                    Some(despawn_package(skill.kind)),
                    None,
                    skill.source,
                    0,
                );
            }
            despawn_stable(&mut commands, &mut identities, skill_entity, *stable);
        }
    }
}

fn bee_skill_consumed_on_hit(kind: BeeSkillKind) -> bool {
    matches!(
        kind,
        BeeSkillKind::WorkerBee | BeeSkillKind::HoneyGlob | BeeSkillKind::HomingSting
    )
}

fn update_skill_repeat_window(skill: &mut ActiveBeeSkill) -> bool {
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

fn bee_presentation_matches_event(event: SimEvent, intent: BeePresentationIntent) -> bool {
    if event.id != intent.event_id || event.id.source != SimEventSource::Entity(intent.entity) {
        return false;
    }
    match intent.kind {
        BeePresentationKind::Lifecycle {
            event: expected, ..
        } => matches!(
            event.kind,
            SimEventKind::AbilityLifecycle { entity, event }
                if entity == intent.entity && event == expected
        ),
        BeePresentationKind::Impact { victim, .. } => {
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

/// Applies a validated render-only bee sidecar from the shared event router.
pub(crate) fn present_bee_event(
    event: SimEvent,
    intent: BeePresentationIntent,
    commands: &mut Commands,
    effect_assets: &EffectAssets,
    feedback: &mut HitEffects,
) -> bool {
    if !bee_presentation_matches_event(event, intent) {
        return false;
    }

    match intent.kind {
        BeePresentationKind::Lifecycle {
            position,
            direction,
            package,
            cue,
            source,
            priority,
            ..
        } => {
            if let Some(package) = package {
                spawn_feedback_package(commands, effect_assets, position, direction, package);
            }
            if let Some(cue) = cue {
                feedback.push_feedback_cue(cue, source, priority);
            }
        }
        BeePresentationKind::Impact {
            position,
            direction,
            package,
            hazard_cue,
            ..
        } => {
            spawn_feedback_package(commands, effect_assets, position, direction, package);
            if hazard_cue {
                feedback.push_feedback_cue("impact_special_hazard", ImpactSource::Hazard, 24);
            }
        }
    }
    true
}

fn update_skill_motion(
    skill: &mut ActiveBeeSkill,
    transform: &mut SimPosition,
    dt: f32,
    targets: &Query<(&Fighter, &SimPosition), With<Fighter>>,
) {
    match skill.kind {
        BeeSkillKind::HoneyPuddle | BeeSkillKind::UltimateSwarm => {}
        BeeSkillKind::HoneyGlob => {
            skill.velocity.y -= BEE_HONEY_GLOB_GRAVITY * dt;
            transform.translation += skill.velocity * dt;
        }
        BeeSkillKind::WorkerBee | BeeSkillKind::HomingSting => {
            if let Some(target_id) = skill.target
                && let Some((_, target_transform)) = targets
                    .iter()
                    .find(|(fighter, _)| fighter.id == target_id.index())
            {
                steer_skill_toward(
                    skill,
                    transform.translation,
                    target_transform.translation + Vec3::Y * 0.85,
                    dt,
                );
            }
            transform.translation += skill.velocity * dt;
        }
    }
}

fn steer_skill_toward(
    skill: &mut ActiveBeeSkill,
    current_position: Vec3,
    target_position: Vec3,
    dt: f32,
) {
    let desired = canonical_math::vec3_normalize_or_zero(target_position - current_position);
    if canonical_math::vec3_length_squared(desired) <= 0.01 {
        return;
    }
    let turn = match skill.kind {
        BeeSkillKind::HomingSting => BEE_HOMING_TURN_RATE,
        BeeSkillKind::WorkerBee => BEE_HOMING_TURN_RATE * 0.45,
        _ => 0.0,
    };
    let speed = canonical_math::vec3_length(skill.velocity);
    skill.velocity = skill
        .velocity
        .lerp(desired * speed, (dt * turn).clamp(0.0, 1.0));
    skill.facing = canonical_math::vec3_normalize_or_zero(skill.velocity);
}

fn bee_skill_impact_profile(
    skill: &ActiveBeeSkill,
    feel: &CombatFeelTuning,
) -> crate::combat::ImpactProfile {
    let mut profile = impact_profile_from_payload_with_feel(
        skill.owner.index(),
        skill.source,
        skill.payload_id,
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

fn bee_skill_overlaps_target(
    skill: &ActiveBeeSkill,
    origin: Vec3,
    target_transform: &SimPosition,
) -> bool {
    match skill.kind {
        BeeSkillKind::HoneyPuddle => {
            let target_position = target_transform.translation;
            let rendered_radius =
                skill.radius * honey_puddle_canonical_scale(skill.age) + FIGHTER_RADIUS;
            debug_assert!(rendered_radius >= 0.0);
            let horizontal_overlap =
                flat_distance_squared(origin, target_position) <= rendered_radius * rendered_radius;
            let vertical_overlap = (target_position.y - origin.y).abs()
                <= BEE_HONEY_PUDDLE_VERTICAL_REACH * skill.size_scale;

            horizontal_overlap && vertical_overlap
        }
        BeeSkillKind::UltimateSwarm => {
            let target_position = target_transform.translation;
            let rendered_radius =
                skill.radius * ultimate_swarm_canonical_scale(skill.age) + FIGHTER_RADIUS;
            debug_assert!(rendered_radius >= 0.0);
            let horizontal_overlap =
                flat_distance_squared(origin, target_position) <= rendered_radius * rendered_radius;
            let vertical_overlap = (target_position.y - origin.y).abs()
                <= BEE_ULTIMATE_SWARM_VERTICAL_REACH * skill.size_scale;

            horizontal_overlap && vertical_overlap
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

pub fn bee_skill_lock_target(
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
    let facing = canonical_math::vec3_normalize_or_zero(facing);
    if canonical_math::vec3_length_squared(facing) <= 0.01 {
        return None;
    }

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
            if distance_squared > BEE_SKILL_LOCK_RANGE * BEE_SKILL_LOCK_RANGE
                || distance_squared <= 0.01 * 0.01
            {
                return None;
            }
            let direction = canonical_math::vec3_normalize_or_zero(offset);
            (direction.dot(facing) >= BEE_SKILL_LOCK_CONE_DOT)
                .then_some((target.fighter_id, distance_squared))
        })
        .min_by(|(fighter_a, distance_a), (fighter_b, distance_b)| {
            distance_a
                .total_cmp(distance_b)
                .then_with(|| fighter_a.cmp(fighter_b))
        })
        .map(|(entity, _)| entity)
}

fn bee_skill_can_hit_target(skill: &ActiveBeeSkill, target: FighterId, state: &MatchState) -> bool {
    target != skill.owner
        && state.combat_target_allowed_for_state(skill.owner.index(), target.index())
}

fn bee_skill_target_for_mode(
    spawn_mode: BeeSkillSpawnMode,
    owner: FighterId,
    origin: Vec3,
    facing: Vec3,
    aim_held: bool,
    state: &MatchState,
    targets: &[BeeSkillTargetSnapshot],
) -> Option<FighterId> {
    match spawn_mode {
        BeeSkillSpawnMode::Standard => {
            bee_skill_lock_target(owner, origin, facing, aim_held, state, targets)
        }
        BeeSkillSpawnMode::AreaSwarm => None,
    }
}

fn bee_skill_side_vec(facing: Vec3) -> Vec3 {
    canonical_math::vec3_normalize_or_zero(Vec3::new(-facing.z, 0.0, facing.x))
}

fn bee_ultimate_swarm_center(
    origin: Vec3,
    facing: Vec3,
    size_scale: f32,
    arena: &ArenaDefinition,
) -> Vec3 {
    let facing = canonical_math::vec3_normalize_or_zero(facing);
    let offset = facing * BEE_ULTIMATE_SWARM_CENTER_OFFSET * bee_skill_size_scale(size_scale);
    let position = origin + offset;
    let ground = ground_height(arena, position.x, position.z);
    Vec3::new(position.x, ground + 0.045, position.z)
}

fn bee_swarm_orbiter_transform(index: usize, age: f32) -> Transform {
    let phase = bee_swarm_orbiter_phase(index) + age * BEE_ULTIMATE_SWARM_ORBIT_SPEED;
    let offset = bee_swarm_orbiter_offset(index, age);
    let tangent = Vec3::new(-phase.sin(), 0.0, phase.cos()).normalize_or_zero();
    Transform::from_translation(offset)
        .with_rotation(projectile_rotation(tangent))
        .with_scale(Vec3::splat(
            BEE_ULTIMATE_SWARM_BEE_SCALE * KENNEY_CUBE_PET_SCALE,
        ))
}

fn bee_swarm_orbiter_offset(index: usize, age: f32) -> Vec3 {
    let phase = bee_swarm_orbiter_phase(index) + age * BEE_ULTIMATE_SWARM_ORBIT_SPEED;
    Vec3::new(
        phase.cos() * BEE_ULTIMATE_SWARM_ORBIT_RADIUS,
        BEE_ULTIMATE_SWARM_BEE_HEIGHT
            + (phase * 1.7 + age * BEE_ULTIMATE_SWARM_BOB_SPEED).sin()
                * BEE_ULTIMATE_SWARM_BOB_HEIGHT,
        phase.sin() * BEE_ULTIMATE_SWARM_ORBIT_RADIUS,
    )
}

fn bee_swarm_orbiter_phase(index: usize) -> f32 {
    std::f32::consts::TAU * index as f32 / BEE_ULTIMATE_SWARM_BEE_COUNT as f32
}

fn target_position(fighter: FighterId, targets: &[BeeSkillTargetSnapshot]) -> Option<Vec3> {
    targets
        .iter()
        .find(|target| target.fighter_id == fighter)
        .map(|target| target.position)
}

fn flat_direction(origin: Vec3, target: Vec3) -> Vec3 {
    canonical_math::vec3_normalize_or_zero(Vec3::new(target.x - origin.x, 0.0, target.z - origin.z))
}

fn flat_distance_squared(a: Vec3, b: Vec3) -> f32 {
    canonical_math::vec2_distance_squared(Vec2::new(a.x, a.z), Vec2::new(b.x, b.z))
}

fn honey_glob_touched_ground(
    skill: &ActiveBeeSkill,
    position: Vec3,
    arena: &ArenaDefinition,
) -> bool {
    if skill.kind != BeeSkillKind::HoneyGlob {
        return false;
    }
    let ground = ground_height(arena, position.x, position.z);
    position.y <= ground + 0.08 && skill.age.as_millis_floor() > 80
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

fn impact_package(kind: BeeSkillKind) -> FeedbackPackageId {
    match kind {
        BeeSkillKind::HoneyPuddle | BeeSkillKind::UltimateSwarm => {
            FeedbackPackageId::SpecialHazardImpact
        }
        _ => FeedbackPackageId::SpecialProjectileImpact,
    }
}

fn despawn_package(kind: BeeSkillKind) -> FeedbackPackageId {
    match kind {
        BeeSkillKind::HoneyPuddle | BeeSkillKind::UltimateSwarm => {
            FeedbackPackageId::SpecialHazardFade
        }
        _ => FeedbackPackageId::SpecialProjectileRecover,
    }
}

fn projectile_rotation(facing: Vec3) -> Quat {
    let facing = facing.normalize_or_zero();
    if facing.length_squared() > 0.01 {
        Quat::from_rotation_arc(Vec3::Z, facing)
    } else {
        Quat::IDENTITY
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim_event::{PresentationEventCursor, PresentationEventRouter, SimEventJournal};

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct FrozenAbilityFixture {
        outcomes: Vec<(FighterId, ContactSourceKind, ContactOutcomeKind)>,
        ability_source_released: bool,
        ability_target_count: usize,
    }

    fn fighter(index: u8) -> FighterId {
        FighterId::new(index).expect("test fighter index should be valid")
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

    fn presentation_intent_at(tick: u64, ordinal: u16) -> BeePresentationIntent {
        let entity = SimEntityId::new(SimEntityKind::BeeSkill, 0, 1);
        BeePresentationIntent {
            event_id: SimEventId {
                tick: SimTick(tick),
                source: SimEventSource::Entity(entity),
                ordinal,
            },
            entity,
            kind: BeePresentationKind::Lifecycle {
                event: AbilityLifecycleEvent::Spawned,
                position: Vec3::ZERO,
                direction: Vec3::X,
                package: Some(FeedbackPackageId::SpecialProjectileStartup),
                cue: Some("release_special_projectile"),
                source: ImpactSource::Projectile,
                priority: 24,
            },
        }
    }

    fn commit_presentation_event(
        journal: &mut SimEventJournal,
        intents: &mut BeePresentationIntentJournal,
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
            .record(BeePresentationIntent { event_id, ..intent })
            .unwrap();
        event_id
    }

    fn accept_fixture_contacts(mut contacts: ResMut<ContactBuffer>) {
        contacts.sort_for_resolution();
        for index in 0..contacts.len() {
            contacts.mark_outcome(index, ContactOutcomeKind::Accepted);
        }
    }

    fn fixture_fighter(id: FighterId, position: Vec3) -> impl Bundle {
        (
            Fighter {
                id: id.index(),
                name: "contact fixture",
                color: Color::WHITE,
                spawn: position,
            },
            FighterStats::default(),
            FighterMotor::default(),
            FighterActionState::default(),
            SimPosition::new(position),
        )
    }

    fn run_frozen_ability_fixture(reverse_allocations: bool) -> FrozenAbilityFixture {
        let mut app = App::new();
        app.insert_resource(ffa_state())
            .insert_resource(ActiveArena::default())
            .init_resource::<SimulationIdentityAllocator>()
            .init_resource::<CombatFeelTuning>()
            .init_resource::<Hitstop>()
            .init_resource::<ContactBuffer>()
            .insert_resource(TickEventBuffer::new(SimTick(41)))
            .add_systems(
                Update,
                (
                    collect_bee_skill_contacts,
                    accept_fixture_contacts,
                    apply_bee_skill_contact_outcomes,
                )
                    .chain(),
            );

        let fighter_order = if reverse_allocations {
            [fighter(2), fighter(1), fighter(0)]
        } else {
            [fighter(0), fighter(1), fighter(2)]
        };
        for fighter_id in fighter_order {
            let position = if fighter_id == fighter(0) {
                Vec3::new(5.0, 0.0, 0.0)
            } else {
                Vec3::ZERO
            };
            app.world_mut().spawn(fixture_fighter(fighter_id, position));
        }

        let spawn_skill = |app: &mut App, skill: ActiveBeeSkill, position: Vec3| {
            let entity = app
                .world_mut()
                .spawn((skill, SimPosition::new(position)))
                .id();
            let stable = app
                .world_mut()
                .resource_mut::<SimulationIdentityAllocator>()
                .try_allocate(SimEntityKind::BeeSkill, entity)
                .unwrap();
            app.world_mut().entity_mut(entity).insert(stable);
            (entity, stable.id())
        };
        let inert = || {
            active_bee_skill(
                BeeSkillKind::HoneyPuddle,
                fighter(0),
                0,
                FighterStyleKind::Anchor,
                AttackPayloadId::BeeHoneyPuddle,
                Vec3::X,
                Vec3::ZERO,
                None,
                1.0,
            )
        };
        let projectile = || {
            active_bee_skill(
                BeeSkillKind::WorkerBee,
                fighter(0),
                0,
                FighterStyleKind::Anchor,
                AttackPayloadId::BeeWorkerSting,
                Vec3::X,
                Vec3::ZERO,
                None,
                1.0,
            )
        };
        let projectile_position = Vec3::Y * (FIGHTER_HEIGHT * 0.58);
        let (projectile_entity, projectile_source) = if reverse_allocations {
            let _ = spawn_skill(&mut app, inert(), Vec3::new(5.0, 0.0, 4.0));
            spawn_skill(&mut app, projectile(), projectile_position)
        } else {
            let projectile = spawn_skill(&mut app, projectile(), projectile_position);
            let _ = spawn_skill(&mut app, inert(), Vec3::new(5.0, 0.0, 4.0));
            projectile
        };

        let strike_profile = bee_skill_impact_profile(
            app.world()
                .get::<ActiveBeeSkill>(projectile_entity)
                .unwrap(),
            app.world().resource::<CombatFeelTuning>(),
        );
        app.world_mut()
            .resource_mut::<ContactBuffer>()
            .push(ContactRecord::new(
                ContactPhase::Strike,
                ContactSourceKind::FighterStrike,
                SimEntityId::new(SimEntityKind::Hitbox, 7, 1),
                Some(fighter(0)),
                fighter(1),
                7,
                3,
                0,
                Vec3::ZERO,
                Vec3::X,
                strike_profile,
                ContactFlags::default(),
            ));

        app.update();

        let contacts = app.world().resource::<ContactBuffer>();
        let outcomes = (0..contacts.len())
            .map(|index| {
                let record = contacts.record(index).unwrap();
                (
                    record.target,
                    record.source_kind,
                    contacts.outcome(index).unwrap().kind,
                )
            })
            .collect::<Vec<_>>();
        let ability_target_count = (0..contacts.len())
            .filter(|index| {
                contacts
                    .record(*index)
                    .is_some_and(|record| record.source.entity() == Some(projectile_source))
            })
            .count();
        let ability_source_released = app
            .world()
            .resource::<SimulationIdentityAllocator>()
            .mapped_entity(projectile_source)
            .is_none()
            && app.world().get_entity(projectile_entity).is_err();

        FrozenAbilityFixture {
            outcomes,
            ability_source_released,
            ability_target_count,
        }
    }

    #[test]
    fn frozen_multi_target_projectile_outcomes_ignore_ecs_and_pool_allocation_order() {
        let forward = run_frozen_ability_fixture(false);
        let reversed = run_frozen_ability_fixture(true);

        assert_eq!(forward, reversed);
        assert_eq!(forward.ability_target_count, 2);
        assert!(forward.ability_source_released);
        assert_eq!(
            forward.outcomes,
            vec![
                (
                    fighter(1),
                    ContactSourceKind::FighterStrike,
                    ContactOutcomeKind::Accepted,
                ),
                (
                    fighter(1),
                    ContactSourceKind::CharacterAbility,
                    ContactOutcomeKind::Accepted,
                ),
                (
                    fighter(2),
                    ContactSourceKind::CharacterAbility,
                    ContactOutcomeKind::Accepted,
                ),
            ]
        );
    }

    fn spawn_test_worker_swarm(
        mut commands: Commands,
        mut identities: ResMut<SimulationIdentityAllocator>,
        mut presentation: BeePresentationEmitter,
        state: Res<MatchState>,
        arena: Res<ActiveArena>,
    ) {
        spawn_bee_skill(
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
            BeeSkillSpawnMode::Standard,
            BeeSkillId::WorkerSwarm,
            &[],
        );
    }

    fn ffa_state() -> MatchState {
        let mut state = MatchState::default();
        state.rules = crate::game_state::RULE_PRESETS[1];
        state.rule_index = 1;
        state.active_slots = [true, true, true, false];
        state.active_fighter_count = 3;
        state
    }

    #[test]
    fn aim_held_lock_target_selects_valid_enemy_in_front() {
        let mut state = MatchState::default();
        state.active_slots = [true, true, true, false];
        state.active_fighter_count = 3;
        let targets = [
            BeeSkillTargetSnapshot {
                fighter_id: fighter(1),
                position: Vec3::new(4.0, 0.0, 0.0),
            },
            BeeSkillTargetSnapshot {
                fighter_id: fighter(2),
                position: Vec3::new(-1.0, 0.0, 0.0),
            },
        ];

        assert_eq!(
            bee_skill_lock_target(fighter(0), Vec3::ZERO, Vec3::X, true, &state, &targets),
            Some(fighter(1))
        );
        assert_eq!(
            bee_skill_lock_target(fighter(0), Vec3::ZERO, Vec3::X, false, &state, &targets),
            None
        );
    }

    #[test]
    fn lock_target_ignores_inactive_and_friendly_slots() {
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
                position: Vec3::new(2.0, 0.0, 0.0),
            },
        ];

        assert_eq!(
            bee_skill_lock_target(fighter(0), Vec3::ZERO, Vec3::X, true, &state, &targets),
            Some(fighter(1))
        );
    }

    #[test]
    fn lock_target_ignores_owner_even_when_ffa_allows_self_targets() {
        let state = ffa_state();
        let targets = [
            BeeSkillTargetSnapshot {
                fighter_id: fighter(0),
                position: Vec3::new(1.0, 0.0, 0.0),
            },
            BeeSkillTargetSnapshot {
                fighter_id: fighter(1),
                position: Vec3::new(3.0, 0.0, 0.0),
            },
        ];

        assert!(state.combat_target_allowed_for_state(0, 0));
        assert_eq!(
            bee_skill_lock_target(fighter(0), Vec3::ZERO, Vec3::X, true, &state, &targets),
            Some(fighter(1))
        );
    }

    #[test]
    fn lock_target_breaks_equal_distance_ties_by_fighter_id() {
        let state = ffa_state();
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
            bee_skill_lock_target(fighter(0), Vec3::ZERO, Vec3::X, true, &state, &targets),
            Some(fighter(1))
        );
    }

    #[test]
    fn bee_skill_hits_reject_owner_even_when_ffa_allows_self_targets() {
        let state = ffa_state();
        let skill = active_bee_skill(
            BeeSkillKind::UltimateSwarm,
            fighter(0),
            0,
            FighterStyleKind::Anchor,
            AttackPayloadId::BeeUltimateSwarmTick,
            Vec3::X,
            Vec3::ZERO,
            None,
            1.0,
        );

        assert!(state.combat_target_allowed_for_state(0, 0));
        assert!(!bee_skill_can_hit_target(&skill, fighter(0), &state));
        assert!(bee_skill_can_hit_target(&skill, fighter(1), &state));
    }

    #[test]
    fn area_swarm_mode_does_not_take_aim_lock_target() {
        let mut state = MatchState::default();
        state.active_slots = [true, true, false, false];
        state.active_fighter_count = 2;
        let targets = [BeeSkillTargetSnapshot {
            fighter_id: fighter(1),
            position: Vec3::new(3.0, 0.0, 0.0),
        }];

        assert_eq!(
            bee_skill_target_for_mode(
                BeeSkillSpawnMode::Standard,
                fighter(0),
                Vec3::ZERO,
                Vec3::X,
                true,
                &state,
                &targets,
            ),
            Some(fighter(1))
        );
        assert_eq!(
            bee_skill_target_for_mode(
                BeeSkillSpawnMode::AreaSwarm,
                fighter(0),
                Vec3::ZERO,
                Vec3::X,
                true,
                &state,
                &targets,
            ),
            None
        );
    }

    #[test]
    fn homing_velocity_turns_toward_captured_target() {
        let mut skill = active_bee_skill(
            BeeSkillKind::HomingSting,
            fighter(0),
            0,
            FighterStyleKind::Anchor,
            AttackPayloadId::BeeHomingSting,
            Vec3::X,
            Vec3::X * BEE_HOMING_SPEED,
            Some(fighter(1)),
            1.0,
        );

        steer_skill_toward(&mut skill, Vec3::ZERO, Vec3::new(0.0, 0.0, 3.0), 0.08);

        assert!(skill.velocity.z > 0.0);
        assert!(skill.velocity.x < BEE_HOMING_SPEED);
    }

    #[test]
    fn puddle_repeat_window_clears_contact_memory() {
        let mut skill = active_bee_skill(
            BeeSkillKind::HoneyPuddle,
            fighter(0),
            0,
            FighterStyleKind::Anchor,
            AttackPayloadId::BeeHoneyPuddle,
            Vec3::X,
            Vec3::ZERO,
            None,
            1.0,
        );
        skill.already_hit.insert(fighter(1));
        skill.repeat_timer = Some(TickTimer::from_ticks(1));
        assert!(update_skill_repeat_window(&mut skill));

        assert!(skill.already_hit.is_empty());
        assert_eq!(skill.repeat_timer, skill.repeat_interval);
    }

    #[test]
    fn headless_bee_spawn_has_only_canonical_components_and_semantic_events() {
        let mut app = App::new();
        app.init_resource::<SimulationIdentityAllocator>()
            .insert_resource(TickEventBuffer::new(SimTick(6)))
            .insert_resource(MatchState::default())
            .insert_resource(ActiveArena::default())
            .add_systems(Update, spawn_test_worker_swarm);

        app.update();

        let world = app.world_mut();
        let mut skills = world.query_filtered::<Entity, With<ActiveBeeSkill>>();
        let entities = skills.iter(world).collect::<Vec<_>>();
        assert_eq!(entities.len(), 2);
        for entity in entities {
            assert!(world.get::<StableSimEntity>(entity).is_some());
            assert!(world.get::<SimPosition>(entity).is_some());
            assert!(world.get::<Transform>(entity).is_none());
            assert!(world.get::<Mesh3d>(entity).is_none());
            assert!(world.get::<SceneRoot>(entity).is_none());
            assert!(world.get::<BeeSkillVisualRoot>(entity).is_none());
        }
        assert!(world.get_resource::<BeeSkillAssets>().is_none());
        assert!(world.get_resource::<EffectAssets>().is_none());
        assert!(world.get_resource::<HitEffects>().is_none());
        assert_eq!(world.resource::<TickEventBuffer>().len(), 2);
    }

    #[test]
    fn bee_presentation_journal_is_bounded_and_rejects_bad_ordinals() {
        let mut intents = BeePresentationIntentJournal::default();
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
    }

    #[test]
    fn bee_events_survive_render_stall_and_rollback_exactly_once() {
        let mut journal = SimEventJournal::default();
        let mut intents = BeePresentationIntentJournal::default();
        for tick in 30..33 {
            commit_presentation_event(&mut journal, &mut intents, tick);
        }

        let mut cursor = PresentationEventCursor::default();
        let mut router = PresentationEventRouter::default();
        let mut presented = Vec::new();
        cursor
            .route_available(&journal, &mut router, Some(SimTick(32)), |event| {
                if let Some(intent) = intents.get(event.id)
                    && bee_presentation_matches_event(event, intent)
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
                    && bee_presentation_matches_event(event, intent)
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
    fn ultimate_swarm_uses_repeating_hazard_tuning() {
        let skill = active_bee_skill(
            BeeSkillKind::UltimateSwarm,
            fighter(0),
            0,
            FighterStyleKind::Anchor,
            AttackPayloadId::BeeUltimateSwarmTick,
            Vec3::X,
            Vec3::ZERO,
            None,
            1.0,
        );

        assert_eq!(skill.payload_id, AttackPayloadId::BeeUltimateSwarmTick);
        assert_eq!(skill.shape_id, AttackShapeId::HazardField);
        assert_eq!(skill.source, ImpactSource::Hazard);
        assert_eq!(
            skill.lifetime,
            TickTimer::from_seconds_ceil(BEE_ULTIMATE_SWARM_LIFETIME)
        );
        assert_eq!(skill.radius, BEE_ULTIMATE_SWARM_RADIUS);
        assert_eq!(
            skill.repeat_interval,
            Some(TickTimer::from_seconds_ceil(BEE_ULTIMATE_SWARM_TICK))
        );
        assert_eq!(skill.repeat_timer, skill.repeat_interval);
    }

    #[test]
    fn honey_puddle_overlap_requires_floor_level_contact() {
        let skill = active_bee_skill(
            BeeSkillKind::HoneyPuddle,
            fighter(0),
            0,
            FighterStyleKind::Anchor,
            AttackPayloadId::BeeHoneyPuddle,
            Vec3::X,
            Vec3::ZERO,
            None,
            1.0,
        );
        let puddle_origin = Vec3::new(0.0, 0.035, 0.0);
        let hit_radius =
            BEE_HONEY_PUDDLE_RADIUS * honey_puddle_canonical_scale(skill.age) + FIGHTER_RADIUS;
        let grounded_target = SimPosition::new(Vec3::new(hit_radius - 0.01, 0.0, 0.0));
        let outside_rendered_body_edge = SimPosition::new(Vec3::new(hit_radius + 0.01, 0.0, 0.0));
        let airborne_target = SimPosition::new(Vec3::new(0.0, FIGHTER_HEIGHT * 2.0, 0.0));

        assert!(bee_skill_overlaps_target(
            &skill,
            puddle_origin,
            &grounded_target
        ));
        assert!(!bee_skill_overlaps_target(
            &skill,
            puddle_origin,
            &outside_rendered_body_edge
        ));
        assert!(!bee_skill_overlaps_target(
            &skill,
            puddle_origin,
            &airborne_target
        ));
    }

    #[test]
    fn mushroom_size_scale_enlarges_bee_skill_collision_radii() {
        let size_scale = crate::constants::ITEM_GIANT_SIZE_MULTIPLIER;
        let cases = [
            (
                BeeSkillKind::WorkerBee,
                AttackPayloadId::BeeWorkerSting,
                BEE_WORKER_RADIUS,
            ),
            (
                BeeSkillKind::HoneyGlob,
                AttackPayloadId::BeeHoneyGlob,
                BEE_HONEY_GLOB_RADIUS,
            ),
            (
                BeeSkillKind::HoneyPuddle,
                AttackPayloadId::BeeHoneyPuddle,
                BEE_HONEY_PUDDLE_RADIUS,
            ),
            (
                BeeSkillKind::HomingSting,
                AttackPayloadId::BeeHomingSting,
                BEE_HOMING_RADIUS,
            ),
            (
                BeeSkillKind::UltimateSwarm,
                AttackPayloadId::BeeUltimateSwarmTick,
                BEE_ULTIMATE_SWARM_RADIUS,
            ),
        ];

        for (kind, payload, base_radius) in cases {
            let skill = active_bee_skill(
                kind,
                fighter(0),
                0,
                FighterStyleKind::Anchor,
                payload,
                Vec3::X,
                Vec3::X,
                None,
                size_scale,
            );

            assert_eq!(skill.size_scale, size_scale);
            assert_eq!(skill.radius, base_radius * size_scale);
        }
    }

    #[test]
    fn mushroom_size_scale_enlarges_bee_skill_visuals() {
        let size_scale = crate::constants::ITEM_GIANT_SIZE_MULTIPLIER;

        assert_vec3_close(
            bee_skill_visual_scale(BeeSkillKind::WorkerBee, size_scale, 0.0),
            Vec3::new(1.25, 0.75, 0.75) * size_scale,
        );
        assert_vec3_close(
            bee_skill_visual_scale(BeeSkillKind::HoneyGlob, size_scale, 0.0),
            Vec3::splat(0.38 * size_scale),
        );
        assert_vec3_close(
            bee_skill_visual_scale(BeeSkillKind::HomingSting, size_scale, 0.0),
            Vec3::splat(size_scale),
        );
        assert_vec3_close(
            bee_skill_visual_scale(BeeSkillKind::HoneyPuddle, size_scale, 0.25),
            Vec3::splat(honey_puddle_visual_pulse(0.25) * size_scale),
        );
        assert_vec3_close(
            bee_skill_visual_scale(BeeSkillKind::UltimateSwarm, size_scale, 0.25),
            Vec3::splat(ultimate_swarm_visual_pulse(0.25) * size_scale),
        );
    }

    #[test]
    fn ultimate_swarm_orbiters_are_evenly_phased() {
        let first = bee_swarm_orbiter_offset(0, 0.0);
        assert_vec3_close(
            first,
            Vec3::new(
                BEE_ULTIMATE_SWARM_ORBIT_RADIUS,
                BEE_ULTIMATE_SWARM_BEE_HEIGHT,
                0.0,
            ),
        );

        for index in 0..BEE_ULTIMATE_SWARM_BEE_COUNT {
            let offset = bee_swarm_orbiter_offset(index, 0.0);
            let horizontal_radius = Vec2::new(offset.x, offset.z).length();
            assert!(
                (horizontal_radius - BEE_ULTIMATE_SWARM_ORBIT_RADIUS).abs() <= 0.0001,
                "orbiter {index} radius was {horizontal_radius}"
            );
        }

        let moved = bee_swarm_orbiter_offset(0, 0.1);
        assert!(moved.z.abs() > 0.1);
    }

    #[test]
    fn honey_asset_exists_for_runtime_loading() {
        assert!(std::path::Path::new("assets/food/kenney_food_kit/honey.glb").exists());
        assert!(std::path::Path::new("assets/characters/kenney_cube_pets/animal-bee.glb").exists());
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn c1_frozen_bee_scales_match_every_v3_reference_tick() {
        let lifetime_ticks = crate::simulation::seconds_to_ticks_ceil(BEE_HONEY_PUDDLE_LIFETIME);
        assert_eq!(lifetime_ticks, 144);
        assert_eq!(
            lifetime_ticks,
            crate::simulation::seconds_to_ticks_ceil(BEE_ULTIMATE_SWARM_LIFETIME)
        );

        for tick in 0..=lifetime_ticks {
            let age = ElapsedTicks::from_ticks(tick);
            assert_eq!(
                honey_puddle_canonical_scale(age).to_bits(),
                honey_puddle_visual_pulse(age.as_seconds()).to_bits(),
                "Honey Puddle reference mismatch at tick {tick}"
            );
            assert_eq!(
                ultimate_swarm_canonical_scale(age).to_bits(),
                ultimate_swarm_visual_pulse(age.as_seconds()).to_bits(),
                "Ultimate Swarm reference mismatch at tick {tick}"
            );
        }
    }

    #[test]
    fn bee_identity_pool_rejects_overflow_without_evicting_live_skill() {
        let mut capacities = [0; SimEntityKind::ALL.len()];
        capacities[SimEntityKind::BeeSkill.code() as usize] = 1;
        let mut identities = SimulationIdentityAllocator::with_capacities(capacities);
        let live = identities
            .try_allocate(SimEntityKind::BeeSkill, local_entity(1))
            .unwrap();

        let overflow = identities
            .try_allocate(SimEntityKind::BeeSkill, local_entity(2))
            .unwrap_err();

        assert_eq!(live.id().kind(), SimEntityKind::BeeSkill);
        assert_eq!(overflow.kind, SimEntityKind::BeeSkill);
        assert_eq!(identities.mapped_entity(live.id()), Some(local_entity(1)));
        assert_eq!(identities.rejected_spawns(SimEntityKind::BeeSkill), 1);
    }
}
