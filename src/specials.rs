use bevy::prelude::*;

use crate::arena::ground_support_for_arena_with_radius;
use crate::arena_defs::{ActiveArena, ArenaDefinition};
use crate::canonical_math;
use crate::combat::{
    HitEffects, ImpactProfile, ImpactSource, can_receive_impact, impact_profile_from_payload,
    impact_profile_from_payload_with_feel, radial_falloff,
};
use crate::components::{
    Fighter, FighterAction, FighterActionState, FighterInput, FighterMotor, FighterSpecialState,
    FighterStats, SimPosition,
};
use crate::constants::*;
use crate::contact_arbitration::{
    ContactBuffer, ContactFlags, ContactOutcomeKind, ContactPhase, ContactRecord, ContactSourceKind,
};
use crate::determinism::{FighterHitMask, FighterId, SimEntityId, SimEntityKind};
use crate::ecs_identity::{SimulationIdentityAllocator, StableSimEntity, despawn_stable};
use crate::effects::{EffectAssets, FeedbackPackageId, spawn_feedback_package};
use crate::equipment::{
    FighterEquipment, LoadoutContext, loadout_special_cooldown_scale, loadout_special_cost_scale,
    loadout_special_stamina_disrupt,
};
use crate::feel::CombatFeelTuning;
use crate::fighter::cancel_dash_slide_for_action;
use crate::game_state::{Hitstop, MatchState};
use crate::rollback::RollbackEventDiscard;
use crate::sim_event::{
    AbilityLifecycleEvent, EventEmitError, MAX_SIM_EVENTS_PER_TICK, SIM_EVENT_HISTORY_TICKS,
    SimEvent, SimEventId, SimEventKind, SimEventSource, TickEventBuffer,
};
use crate::simulation::{ElapsedTicks, SIM_HZ_U32, SimTick, TickTimer};
use crate::styles::{FighterStyle, FighterStyleKind};
use crate::techniques::{
    AttackPayloadId, AttackShapeId, MsTimingWindow, attack_payload_definition,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpecialKind {
    Projectile,
    Trap,
    Shockwave,
    Hazard,
}

#[derive(Clone, Copy)]
struct SpecialDefinition {
    #[allow(dead_code)]
    cost: f32,
    lifetime: f32,
    radius: f32,
    payload_id: AttackPayloadId,
    shape_id: AttackShapeId,
    source: ImpactSource,
    timing: SpecialTimingDef,
    presentation: SpecialPresentationDef,
    guard_stamina_damage: f32,
}

#[derive(Clone, Copy)]
struct SpecialTimingDef {
    launch_ms: u32,
    active_window: MsTimingWindow,
    repeat_ms: Option<u32>,
    aftermath_ms: Option<u32>,
    startup_cue: &'static str,
    active_cue: &'static str,
    aftermath_cue: &'static str,
}

#[derive(Clone, Copy)]
struct SpecialPresentationDef {
    startup_package: FeedbackPackageId,
    active_package: FeedbackPackageId,
    repeat_package: Option<FeedbackPackageId>,
    impact_package: FeedbackPackageId,
    aftermath_package: FeedbackPackageId,
    despawn_package: FeedbackPackageId,
}

#[derive(Component)]
pub struct ActiveSpecial {
    pub kind: SpecialKind,
    pub owner: FighterId,
    pub owner_style: FighterStyleKind,
    pub payload_id: AttackPayloadId,
    pub shape_id: AttackShapeId,
    pub source: ImpactSource,
    pub facing: Vec3,
    pub velocity: Vec3,
    pub lifetime: TickTimer,
    pub age: ElapsedTicks,
    pub total_lifetime_ms: u32,
    pub radius: f32,
    pub grace: TickTimer,
    pub launch_ms: u32,
    pub active_window: MsTimingWindow,
    pub repeat_ms: Option<u32>,
    pub next_repeat_ms: Option<u32>,
    pub active_feedback_sent: bool,
    pub aftermath_ms: Option<u32>,
    pub aftermath_feedback_sent: bool,
    pub active_cue: &'static str,
    pub aftermath_cue: &'static str,
    pub active_package: FeedbackPackageId,
    pub repeat_package: Option<FeedbackPackageId>,
    pub impact_package: FeedbackPackageId,
    pub aftermath_package: FeedbackPackageId,
    pub despawn_package: FeedbackPackageId,
    pub stamina_disrupt: f32,
    pub guard_stamina_damage: f32,
    pub already_hit: FighterHitMask,
}

/// Renderer-facing work paired with one semantic special event.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum SpecialPresentationKind {
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
        stamina_disrupt_cue: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SpecialPresentationIntent {
    pub event_id: SimEventId,
    pub entity: SimEntityId,
    pub kind: SpecialPresentationKind,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct SpecialPresentationIntentSlot {
    tick: SimTick,
    len: u16,
    occupied: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SpecialPresentationIntentMetrics {
    pub recorded: u64,
    pub replaced: u64,
    pub rejected: u64,
    pub discarded: u64,
}

/// Fixed-capacity, rollback-discardable special-presentation sidecar.
#[derive(Resource, Clone, Debug)]
pub struct SpecialPresentationIntentJournal {
    slots: [SpecialPresentationIntentSlot; SIM_EVENT_HISTORY_TICKS],
    intents: Box<[Option<SpecialPresentationIntent>]>,
    len: usize,
    metrics: SpecialPresentationIntentMetrics,
}

impl Default for SpecialPresentationIntentJournal {
    fn default() -> Self {
        Self {
            slots: [SpecialPresentationIntentSlot::default(); SIM_EVENT_HISTORY_TICKS],
            intents: vec![None; SIM_EVENT_HISTORY_TICKS * MAX_SIM_EVENTS_PER_TICK]
                .into_boxed_slice(),
            len: 0,
            metrics: SpecialPresentationIntentMetrics::default(),
        }
    }
}

impl SpecialPresentationIntentJournal {
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
    pub const fn metrics(&self) -> SpecialPresentationIntentMetrics {
        self.metrics
    }

    pub(crate) fn record(
        &mut self,
        intent: SpecialPresentationIntent,
    ) -> Result<(), EventEmitError> {
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
            *slot = SpecialPresentationIntentSlot {
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

    pub(crate) fn get(&self, event_id: SimEventId) -> Option<SpecialPresentationIntent> {
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
            self.slots[slot_index] = SpecialPresentationIntentSlot::default();
            self.len = self.len.saturating_sub(usize::from(slot.len));
            self.metrics.discarded = self.metrics.discarded.saturating_add(u64::from(slot.len));
        }
    }
}

impl RollbackEventDiscard for SpecialPresentationIntentJournal {
    fn discard_after(&mut self, retained_through: SimTick) {
        Self::discard_after(self, retained_through);
    }
}

#[derive(Resource, Default)]
pub struct SpecialAssets {
    projectile_mesh: Handle<Mesh>,
    trap_mesh: Handle<Mesh>,
    shockwave_mesh: Handle<Mesh>,
    hazard_mesh: Handle<Mesh>,
    projectile_material: Handle<StandardMaterial>,
    trap_material: Handle<StandardMaterial>,
    shockwave_material: Handle<StandardMaterial>,
    hazard_material: Handle<StandardMaterial>,
}

/// Render-local marker attached only by the Update-side visual projector.
#[derive(Component)]
pub(crate) struct SpecialVisualRoot;

pub fn setup_special_assets(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.insert_resource(SpecialAssets {
        projectile_mesh: meshes.add(Sphere::new(0.24).mesh().uv(16, 8)),
        trap_mesh: meshes.add(Cylinder::new(0.52, 0.08)),
        shockwave_mesh: meshes.add(Torus::new(0.48, 0.035)),
        hazard_mesh: meshes.add(Cylinder::new(0.72, 0.1)),
        projectile_material: materials.add(StandardMaterial {
            base_color: Color::srgb(0.45, 0.95, 1.0),
            emissive: LinearRgba::rgb(0.05, 0.22, 0.28),
            ..default()
        }),
        trap_material: materials.add(StandardMaterial {
            base_color: Color::srgb(1.0, 0.72, 0.18),
            emissive: LinearRgba::rgb(0.2, 0.08, 0.01),
            ..default()
        }),
        shockwave_material: materials.add(StandardMaterial {
            base_color: Color::srgba(0.8, 1.0, 0.72, 0.72),
            emissive: LinearRgba::rgb(0.08, 0.2, 0.05),
            alpha_mode: AlphaMode::Blend,
            ..default()
        }),
        hazard_material: materials.add(StandardMaterial {
            base_color: Color::srgba(0.25, 0.95, 0.48, 0.7),
            emissive: LinearRgba::rgb(0.02, 0.2, 0.06),
            alpha_mode: AlphaMode::Blend,
            ..default()
        }),
    });
}

pub fn handle_special_inputs(
    hitstop: Res<Hitstop>,
    active_arena: Res<ActiveArena>,
    mut commands: Commands,
    mut identities: ResMut<SimulationIdentityAllocator>,
    mut sim_events: ResMut<TickEventBuffer>,
    mut presentation_intents: Option<ResMut<SpecialPresentationIntentJournal>>,
    mut fighters: Query<(
        &Fighter,
        &mut FighterInput,
        &mut FighterMotor,
        &mut FighterActionState,
        &mut FighterSpecialState,
        &FighterStyle,
        &FighterEquipment,
        &SimPosition,
    )>,
) {
    if hitstop.active() {
        return;
    }

    for fighter_id in FighterId::ALL {
        let Some((
            _fighter,
            mut input,
            mut motor,
            mut action,
            mut special_state,
            style,
            equipment,
            position,
        )) = fighters
            .iter_mut()
            .find(|(fighter, ..)| fighter.id == fighter_id.index())
        else {
            continue;
        };
        if !input.special || special_state.cooldown.active() || !can_cast_special(action.action) {
            continue;
        }

        let kind = requested_special_kind(&input);
        let loadout = LoadoutContext::new(style.kind, equipment.kind);
        input.special = false;
        input.heavy = false;
        input.grab = false;

        let spawned = spawn_special(
            &mut commands,
            &mut identities,
            &mut sim_events,
            presentation_intents.as_deref_mut(),
            fighter_id,
            kind,
            style.kind,
            loadout,
            position.translation,
            motor.facing,
            active_arena.definition(),
        );
        if !spawned {
            continue;
        }

        cancel_dash_slide_for_action(&mut motor);
        special_state
            .cooldown
            .set(TickTimer::from_seconds_ceil(styled_special_cooldown(
                loadout,
            )));
        set_special_action(&mut action);
    }
}

fn special_cue(kind: SpecialKind) -> &'static str {
    special_definition(kind).timing.startup_cue
}

pub fn tick_special_cooldowns(
    hitstop: Res<Hitstop>,
    mut fighters: Query<&mut FighterSpecialState>,
) {
    if hitstop.active() {
        return;
    }

    for mut special_state in &mut fighters {
        special_state.cooldown.tick();
    }
}

fn can_cast_special(action: FighterAction) -> bool {
    matches!(
        action,
        FighterAction::Idle | FighterAction::Moving | FighterAction::Guarding
    )
}

fn requested_special_kind(input: &FighterInput) -> SpecialKind {
    if input.guard {
        SpecialKind::Trap
    } else if input.heavy {
        SpecialKind::Hazard
    } else if input.grab {
        SpecialKind::Shockwave
    } else {
        SpecialKind::Projectile
    }
}

fn special_definition(kind: SpecialKind) -> SpecialDefinition {
    match kind {
        SpecialKind::Projectile => SpecialDefinition {
            cost: SPECIAL_PROJECTILE_COST,
            lifetime: SPECIAL_PROJECTILE_LIFETIME,
            radius: SPECIAL_PROJECTILE_RADIUS,
            payload_id: AttackPayloadId::SpecialProjectile,
            shape_id: AttackShapeId::ProjectileBolt,
            source: ImpactSource::Projectile,
            timing: SpecialTimingDef {
                launch_ms: 90,
                active_window: MsTimingWindow::closed(120, 980),
                repeat_ms: None,
                aftermath_ms: Some(1120),
                startup_cue: "startup_special_projectile",
                active_cue: "release_special_projectile",
                aftermath_cue: "recover_special_projectile",
            },
            presentation: SpecialPresentationDef {
                startup_package: FeedbackPackageId::SpecialProjectileStartup,
                active_package: FeedbackPackageId::SpecialProjectileRelease,
                repeat_package: None,
                impact_package: FeedbackPackageId::SpecialProjectileImpact,
                aftermath_package: FeedbackPackageId::SpecialProjectileRecover,
                despawn_package: FeedbackPackageId::SpecialProjectileRecover,
            },
            guard_stamina_damage: 18.0,
        },
        SpecialKind::Trap => SpecialDefinition {
            cost: SPECIAL_TRAP_COST,
            lifetime: SPECIAL_TRAP_LIFETIME,
            radius: SPECIAL_TRAP_RADIUS,
            payload_id: AttackPayloadId::SpecialTrap,
            shape_id: AttackShapeId::TrapPlate,
            source: ImpactSource::Trap,
            timing: SpecialTimingDef {
                launch_ms: 0,
                active_window: MsTimingWindow::open_ended(260),
                repeat_ms: None,
                aftermath_ms: Some(520),
                startup_cue: "startup_special_trap",
                active_cue: "arm_special_trap",
                aftermath_cue: "recover_special_trap",
            },
            presentation: SpecialPresentationDef {
                startup_package: FeedbackPackageId::SpecialTrapStartup,
                active_package: FeedbackPackageId::SpecialTrapArm,
                repeat_package: None,
                impact_package: FeedbackPackageId::SpecialTrapImpact,
                aftermath_package: FeedbackPackageId::SpecialTrapRecover,
                despawn_package: FeedbackPackageId::SpecialTrapRecover,
            },
            guard_stamina_damage: 22.0,
        },
        SpecialKind::Shockwave => SpecialDefinition {
            cost: SPECIAL_SHOCKWAVE_COST,
            lifetime: SPECIAL_SHOCKWAVE_LIFETIME,
            radius: SPECIAL_SHOCKWAVE_RADIUS,
            payload_id: AttackPayloadId::SpecialShockwave,
            shape_id: AttackShapeId::ShockwaveRing,
            source: ImpactSource::Shockwave,
            timing: SpecialTimingDef {
                launch_ms: 0,
                active_window: MsTimingWindow::closed(70, 300),
                repeat_ms: None,
                aftermath_ms: Some(300),
                startup_cue: "startup_special_shockwave",
                active_cue: "release_special_shockwave",
                aftermath_cue: "recover_special_shockwave",
            },
            presentation: SpecialPresentationDef {
                startup_package: FeedbackPackageId::SpecialShockwaveStartup,
                active_package: FeedbackPackageId::SpecialShockwaveRelease,
                repeat_package: None,
                impact_package: FeedbackPackageId::SpecialShockwaveImpact,
                aftermath_package: FeedbackPackageId::SpecialShockwaveRecover,
                despawn_package: FeedbackPackageId::SpecialShockwaveRecover,
            },
            guard_stamina_damage: 24.0,
        },
        SpecialKind::Hazard => SpecialDefinition {
            cost: SPECIAL_HAZARD_COST,
            lifetime: SPECIAL_HAZARD_LIFETIME,
            radius: SPECIAL_HAZARD_RADIUS,
            payload_id: AttackPayloadId::SpecialHazard,
            shape_id: AttackShapeId::HazardField,
            source: ImpactSource::Hazard,
            timing: SpecialTimingDef {
                launch_ms: 0,
                active_window: MsTimingWindow::closed(340, 3600),
                repeat_ms: Some(420),
                aftermath_ms: Some(3600),
                startup_cue: "startup_special_hazard",
                active_cue: "pulse_special_hazard",
                aftermath_cue: "fade_special_hazard",
            },
            presentation: SpecialPresentationDef {
                startup_package: FeedbackPackageId::SpecialHazardStartup,
                active_package: FeedbackPackageId::SpecialHazardPulse,
                repeat_package: Some(FeedbackPackageId::SpecialHazardPulse),
                impact_package: FeedbackPackageId::SpecialHazardImpact,
                aftermath_package: FeedbackPackageId::SpecialHazardFade,
                despawn_package: FeedbackPackageId::SpecialHazardFade,
            },
            guard_stamina_damage: 14.0,
        },
    }
}

fn set_special_action(action: &mut FighterActionState) {
    action.action = FighterAction::SpecialCast;
    action.elapsed.reset();
    action.hitbox_spawned = false;
    action.queued_combo = false;
    action.queued_technique = None;
    action.queued_button = None;
    action.buffered_button = None;
    action.buffered_button_elapsed.reset();
    action.timeline_events_fired = 0;
    action.reaction_getup_ms = None;
    action.reaction_recover_ms = None;
    action.clear_reaction_visual();
}

fn spawn_special(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    sim_events: &mut TickEventBuffer,
    presentation_intents: Option<&mut SpecialPresentationIntentJournal>,
    owner: FighterId,
    kind: SpecialKind,
    owner_style: FighterStyleKind,
    loadout: LoadoutContext,
    origin: Vec3,
    facing: Vec3,
    arena: &ArenaDefinition,
) -> bool {
    let facing = canonical_math::vec3_normalize_or_zero(facing);
    let definition = special_definition(kind);
    let payload = attack_payload_definition(definition.payload_id);
    debug_assert_eq!(payload.shape_id, definition.shape_id);
    let (position, velocity, lifetime, radius) = match kind {
        SpecialKind::Projectile => (
            origin + Vec3::Y * 0.9 + facing * 0.75,
            facing * SPECIAL_PROJECTILE_SPEED,
            definition.lifetime,
            definition.radius,
        ),
        SpecialKind::Trap => {
            let flat = origin + facing * 0.85;
            let ground = ground_support_for_arena_with_radius(arena, flat.x, flat.z, 0.0)
                .height()
                .unwrap_or(ARENA_TOP_Y);
            (
                Vec3::new(flat.x, ground + 0.06, flat.z),
                Vec3::ZERO,
                definition.lifetime,
                definition.radius,
            )
        }
        SpecialKind::Shockwave => (
            origin + Vec3::Y * 0.08,
            Vec3::ZERO,
            definition.lifetime,
            definition.radius,
        ),
        SpecialKind::Hazard => {
            let flat = origin + facing * 1.15;
            let ground = ground_support_for_arena_with_radius(arena, flat.x, flat.z, 0.0)
                .height()
                .unwrap_or(ARENA_TOP_Y);
            (
                Vec3::new(flat.x, ground + 0.08, flat.z),
                Vec3::ZERO,
                definition.lifetime,
                definition.radius,
            )
        }
    };

    let entity = commands.spawn_empty().id();
    let stable = match identities.try_allocate(SimEntityKind::Special, entity) {
        Ok(stable) => stable,
        Err(_) => {
            commands.entity(entity).despawn();
            return false;
        }
    };
    commands.entity(entity).insert((
        stable,
        SimPosition::new(position),
        ActiveSpecial {
            kind,
            owner,
            owner_style,
            payload_id: definition.payload_id,
            shape_id: definition.shape_id,
            source: definition.source,
            facing,
            velocity,
            lifetime: TickTimer::from_seconds_ceil(lifetime),
            age: ElapsedTicks::ZERO,
            total_lifetime_ms: (definition.lifetime * 1000.0) as u32,
            radius,
            grace: TickTimer::from_seconds_ceil(SPECIAL_OWNER_GRACE),
            launch_ms: definition.timing.launch_ms,
            active_window: definition.timing.active_window,
            repeat_ms: definition.timing.repeat_ms,
            next_repeat_ms: definition
                .timing
                .repeat_ms
                .map(|repeat_ms| definition.timing.active_window.start_ms + repeat_ms),
            active_feedback_sent: false,
            aftermath_ms: definition.timing.aftermath_ms,
            aftermath_feedback_sent: false,
            active_cue: definition.timing.active_cue,
            aftermath_cue: definition.timing.aftermath_cue,
            active_package: definition.presentation.active_package,
            repeat_package: definition.presentation.repeat_package,
            impact_package: definition.presentation.impact_package,
            aftermath_package: definition.presentation.aftermath_package,
            despawn_package: definition.presentation.despawn_package,
            stamina_disrupt: loadout_special_stamina_disrupt(loadout),
            guard_stamina_damage: definition.guard_stamina_damage,
            already_hit: FighterHitMask::default(),
        },
    ));
    emit_special_lifecycle(
        sim_events,
        presentation_intents,
        stable.id(),
        AbilityLifecycleEvent::Spawned,
        origin,
        facing,
        Some(definition.presentation.startup_package),
        Some(special_cue(kind)),
        ImpactSource::MatchFlow,
        20,
    );
    true
}

pub fn collect_special_contacts(
    identities: Res<SimulationIdentityAllocator>,
    state: Res<MatchState>,
    feel: Res<CombatFeelTuning>,
    hitstop: Res<Hitstop>,
    mut contact_buffer: ResMut<ContactBuffer>,
    mut sim_events: ResMut<TickEventBuffer>,
    mut special_presentation_intents: Option<ResMut<SpecialPresentationIntentJournal>>,
    mut specials: Query<(&StableSimEntity, &mut ActiveSpecial, &mut SimPosition), Without<Fighter>>,
    fighters: Query<
        (&Fighter, &FighterStats, &FighterActionState, &SimPosition),
        Without<ActiveSpecial>,
    >,
) {
    if hitstop.active() {
        return;
    }

    let dt = 1.0 / SIM_HZ_U32 as f32;
    for index in 0..identities.capacity(SimEntityKind::Special) {
        let Some((stable_id, special_entity)) = special_pool_entry(&identities, index) else {
            continue;
        };
        let Ok((stable, mut special, mut position)) = specials.get_mut(special_entity) else {
            continue;
        };
        if stable.id() != stable_id {
            continue;
        }
        special.age.advance();
        special.lifetime.tick();
        special.grace.tick();
        let age_ms = special.age.as_millis_floor();

        update_special_timing_feedback(
            &mut special,
            age_ms,
            stable_id,
            position.translation,
            &mut sim_events,
            special_presentation_intents.as_deref_mut(),
        );
        update_special_repeat_window(
            &mut special,
            age_ms,
            stable_id,
            position.translation,
            &mut sim_events,
            special_presentation_intents.as_deref_mut(),
        );

        if age_ms >= special.launch_ms {
            position.translation += special.velocity * dt;
        }
        let active_radius = active_special_radius(&special);
        for target_id in FighterId::ALL {
            let Some((target, stats, action, target_position)) = fighters
                .iter()
                .find(|(fighter, ..)| fighter.id == target_id.index())
            else {
                continue;
            };
            if target_id == special.owner && special.grace.active() {
                continue;
            }
            if !special_active_at(&special, age_ms) {
                continue;
            }
            if !state.combat_target_allowed_for_state(special.owner.index(), target.id) {
                continue;
            }
            if special.already_hit.contains(target_id)
                || !can_receive_impact(&stats, &action)
                || !special_overlaps_target(
                    &special,
                    active_radius,
                    position.translation,
                    target_position,
                )
            {
                continue;
            }

            let falloff = if matches!(special.kind, SpecialKind::Shockwave) {
                radial_falloff(
                    flat_distance(position.translation, target_position.translation),
                    active_radius,
                )
            } else {
                1.0
            };
            let profile = special_impact_profile_with_feel(&special, falloff, &feel);
            let _ = contact_buffer.push(ContactRecord::new(
                ContactPhase::Strike,
                ContactSourceKind::GenericSpecial,
                stable_id,
                Some(special.owner),
                target_id,
                special.payload_id as u16,
                special.shape_id as u16,
                0,
                target_position.translation,
                position.translation,
                profile,
                ContactFlags::default(),
            ));
        }
    }
}

/// Applies special hit memory and source lifecycle only after the shared
/// frozen contact batch has resolved.
pub fn apply_special_contact_outcomes(
    mut commands: Commands,
    mut identities: ResMut<SimulationIdentityAllocator>,
    active_arena: Res<ActiveArena>,
    mut contact_buffer: ResMut<ContactBuffer>,
    mut sim_events: ResMut<TickEventBuffer>,
    mut presentation_intents: Option<ResMut<SpecialPresentationIntentJournal>>,
    mut specials: Query<(&StableSimEntity, &mut ActiveSpecial, &SimPosition), Without<Fighter>>,
    mut fighters: Query<(&Fighter, &mut FighterStats), Without<ActiveSpecial>>,
) {
    for contact_index in 0..contact_buffer.len() {
        let Some(contact) = contact_buffer.record(contact_index) else {
            continue;
        };
        if contact.source_kind != ContactSourceKind::GenericSpecial {
            continue;
        }
        let Some(source) = contact.source.entity() else {
            continue;
        };
        if source.kind() != SimEntityKind::Special {
            continue;
        }
        let Some(source_entity) = identities.mapped_entity(source) else {
            contact_buffer.mark_outcome(contact_index, ContactOutcomeKind::Invalidated);
            continue;
        };
        let Ok((stable, mut special, _)) = specials.get_mut(source_entity) else {
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

        special.already_hit.insert(contact.target);
        if matches!(special.kind, SpecialKind::Projectile | SpecialKind::Trap) {
            special.lifetime.clear();
        }
        if special.stamina_disrupt > 0.0 {
            let active_radius = active_special_radius(&special);
            let falloff = if special.kind == SpecialKind::Shockwave {
                radial_falloff(
                    flat_distance(contact.origin.to_vec3(), contact.contact_point.to_vec3()),
                    active_radius,
                )
            } else {
                1.0
            };
            if let Some((_, mut stats)) = fighters
                .iter_mut()
                .find(|(fighter, _)| fighter.id == contact.target.index())
            {
                stats.stamina =
                    (stats.stamina - special.stamina_disrupt * falloff.max(0.5)).max(0.0);
            }
        }
        if let (Some(event_id), Some(intents)) = (outcome.event_id, presentation_intents.as_mut()) {
            let _ = intents.record(SpecialPresentationIntent {
                event_id,
                entity: source,
                kind: SpecialPresentationKind::Impact {
                    victim: contact.target,
                    position: contact.contact_point.to_vec3() + Vec3::Y * (FIGHTER_HEIGHT * 0.58),
                    direction: special.facing,
                    package: special.impact_package,
                    stamina_disrupt_cue: special.stamina_disrupt > 0.0,
                },
            });
        }
    }

    for index in 0..identities.capacity(SimEntityKind::Special) {
        let Some((stable_id, special_entity)) = special_pool_entry(&identities, index) else {
            continue;
        };
        let Ok((stable, special, position)) = specials.get_mut(special_entity) else {
            continue;
        };
        if stable.id() != stable_id {
            continue;
        }
        let hit_this_tick = (0..contact_buffer.len()).any(|contact_index| {
            contact_buffer
                .record(contact_index)
                .filter(|contact| contact.source.entity() == Some(stable_id))
                .and_then(|_| contact_buffer.outcome(contact_index))
                .is_some_and(|outcome| {
                    matches!(
                        outcome.kind,
                        ContactOutcomeKind::Accepted | ContactOutcomeKind::Guarded
                    )
                })
        });
        if special.lifetime.active()
            && !should_despawn_special(position.translation, active_arena.definition())
        {
            continue;
        }
        if !hit_this_tick {
            emit_special_lifecycle(
                &mut sim_events,
                presentation_intents.as_deref_mut(),
                stable_id,
                AbilityLifecycleEvent::Despawned,
                position.translation,
                special.facing,
                Some(special.despawn_package),
                None,
                special.source,
                0,
            );
        }
        let stable = *stable;
        despawn_stable(&mut commands, &mut identities, special_entity, stable);
    }
}

fn special_pool_entry(
    identities: &SimulationIdentityAllocator,
    index: u32,
) -> Option<(SimEntityId, Entity)> {
    identities.entry_at(SimEntityKind::Special, index)
}

/// Attaches render assets to canonical special roots restored into a client.
/// This system is never scheduled by a headless authority.
pub fn attach_missing_special_visuals(
    mut commands: Commands,
    assets: Res<SpecialAssets>,
    specials: Query<
        (Entity, &ActiveSpecial, &SimPosition, Option<&Transform>),
        Without<SpecialVisualRoot>,
    >,
) {
    for (entity, special, position, transform) in &specials {
        let (mesh, material, name) = match special.kind {
            SpecialKind::Projectile => (
                assets.projectile_mesh.clone(),
                assets.projectile_material.clone(),
                "Pulse Dart",
            ),
            SpecialKind::Trap => (
                assets.trap_mesh.clone(),
                assets.trap_material.clone(),
                "Trip Plate",
            ),
            SpecialKind::Shockwave => (
                assets.shockwave_mesh.clone(),
                assets.shockwave_material.clone(),
                "Snap Wave",
            ),
            SpecialKind::Hazard => (
                assets.hazard_mesh.clone(),
                assets.hazard_material.clone(),
                "Drift Field",
            ),
        };
        let mut entity_commands = commands.entity(entity);
        if transform.is_none() {
            entity_commands.insert(Transform::from_translation(position.translation));
        }
        entity_commands.insert((
            Mesh3d(mesh),
            MeshMaterial3d(material),
            SpecialVisualRoot,
            Name::new(name),
        ));
    }
}

/// Derives non-canonical rotation and scale from canonical age in Update.
pub fn sync_special_visuals(
    mut specials: Query<(&ActiveSpecial, &SimPosition, &mut Transform), With<SpecialVisualRoot>>,
) {
    for (special, position, mut transform) in &mut specials {
        transform.translation = position.translation;
        update_special_visual(special, &mut transform);
    }
}

fn update_special_visual(special: &ActiveSpecial, transform: &mut Transform) {
    match special.kind {
        SpecialKind::Projectile => {
            let launch_ticks = ((special.launch_ms as u64 * u64::from(SIM_HZ_U32)) / 1000) as f32;
            let age_ticks = special.age.get() as f32;
            let prelaunch_ticks = age_ticks.min(launch_ticks);
            let active_ticks = (age_ticks - launch_ticks).max(0.0);
            transform.rotation =
                Quat::from_rotation_y(prelaunch_ticks * 0.04 + active_ticks * 0.18);
            transform.scale = Vec3::ONE;
        }
        SpecialKind::Shockwave => {
            let t = special_active_progress(special);
            transform.scale = Vec3::splat(0.35 + t * SPECIAL_SHOCKWAVE_RADIUS * 2.0);
        }
        SpecialKind::Hazard => {
            let age_seconds = special.age.as_seconds();
            let pulse = if special_active_at(special, special.age.as_millis_floor()) {
                1.0 + (age_seconds * 9.0).sin().abs() * 0.16
            } else {
                0.74 + (age_seconds * 5.0).sin().abs() * 0.08
            };
            transform.scale = Vec3::splat(pulse);
        }
        SpecialKind::Trap => {
            transform.rotation = Quat::from_rotation_y(special.age.get() as f32 * 0.04);
            transform.scale = if special_active_at(special, special.age.as_millis_floor()) {
                Vec3::ONE
            } else {
                Vec3::splat(0.72)
            };
        }
    }
}

fn active_special_radius(special: &ActiveSpecial) -> f32 {
    if special.kind == SpecialKind::Shockwave {
        let t = special_active_progress(special);
        (SPECIAL_SHOCKWAVE_RADIUS * t).max(0.45)
    } else {
        special.radius
    }
}

fn update_special_timing_feedback(
    special: &mut ActiveSpecial,
    age_ms: u32,
    entity: SimEntityId,
    position: Vec3,
    sim_events: &mut TickEventBuffer,
    mut presentation_intents: Option<&mut SpecialPresentationIntentJournal>,
) {
    if !special.active_feedback_sent && age_ms >= special.active_window.start_ms {
        emit_special_lifecycle(
            sim_events,
            presentation_intents.as_deref_mut(),
            entity,
            AbilityLifecycleEvent::Activated,
            position,
            special.facing,
            Some(special.active_package),
            Some(special.active_cue),
            special.source,
            28,
        );
        special.active_feedback_sent = true;
    }
    if let Some(aftermath_ms) = special.aftermath_ms
        && !special.aftermath_feedback_sent
        && age_ms >= aftermath_ms
    {
        emit_special_lifecycle(
            sim_events,
            presentation_intents.as_deref_mut(),
            entity,
            AbilityLifecycleEvent::Aftermath,
            position,
            special.facing,
            Some(special.aftermath_package),
            Some(special.aftermath_cue),
            special.source,
            22,
        );
        special.aftermath_feedback_sent = true;
    }
}

fn update_special_repeat_window(
    special: &mut ActiveSpecial,
    age_ms: u32,
    entity: SimEntityId,
    position: Vec3,
    sim_events: &mut TickEventBuffer,
    presentation_intents: Option<&mut SpecialPresentationIntentJournal>,
) {
    if advance_special_repeat_window(special, age_ms) {
        emit_special_lifecycle(
            sim_events,
            presentation_intents,
            entity,
            AbilityLifecycleEvent::Repeated,
            position,
            special.facing,
            special.repeat_package,
            Some(special.active_cue),
            special.source,
            24,
        );
    }
}

fn advance_special_repeat_window(special: &mut ActiveSpecial, age_ms: u32) -> bool {
    let Some(repeat_ms) = special.repeat_ms else {
        return false;
    };
    let Some(mut next_repeat_ms) = special.next_repeat_ms else {
        return false;
    };
    if !special_active_at(special, age_ms) {
        return false;
    }
    let mut repeated = false;
    while age_ms >= next_repeat_ms {
        special.already_hit.clear();
        repeated = true;
        next_repeat_ms += repeat_ms;
    }
    special.next_repeat_ms = Some(next_repeat_ms);
    repeated
}

#[allow(clippy::too_many_arguments)]
fn emit_special_lifecycle(
    sim_events: &mut TickEventBuffer,
    presentation_intents: Option<&mut SpecialPresentationIntentJournal>,
    entity: SimEntityId,
    event: AbilityLifecycleEvent,
    position: Vec3,
    direction: Vec3,
    package: Option<FeedbackPackageId>,
    cue: Option<&'static str>,
    source: ImpactSource,
    priority: u8,
) {
    let Ok(event_id) = sim_events.emit(
        SimEventSource::Entity(entity),
        SimEventKind::AbilityLifecycle { entity, event },
    ) else {
        return;
    };
    if let Some(intents) = presentation_intents {
        let _ = intents.record(SpecialPresentationIntent {
            event_id,
            entity,
            kind: SpecialPresentationKind::Lifecycle {
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

fn special_presentation_matches_event(event: SimEvent, intent: SpecialPresentationIntent) -> bool {
    if event.id != intent.event_id || event.id.source != SimEventSource::Entity(intent.entity) {
        return false;
    }
    match intent.kind {
        SpecialPresentationKind::Lifecycle {
            event: expected, ..
        } => matches!(
            event.kind,
            SimEventKind::AbilityLifecycle { entity, event }
                if entity == intent.entity && event == expected
        ),
        SpecialPresentationKind::Impact { victim, .. } => {
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

/// Applies a validated render-only special sidecar from the shared event router.
pub(crate) fn present_special_event(
    event: SimEvent,
    intent: SpecialPresentationIntent,
    commands: &mut Commands,
    effect_assets: &EffectAssets,
    feedback: &mut HitEffects,
) -> bool {
    if !special_presentation_matches_event(event, intent) {
        return false;
    }

    match intent.kind {
        SpecialPresentationKind::Lifecycle {
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
        SpecialPresentationKind::Impact {
            position,
            direction,
            package,
            stamina_disrupt_cue,
            ..
        } => {
            spawn_feedback_package(commands, effect_assets, position, direction, package);
            if stamina_disrupt_cue {
                feedback.push_feedback_cue("special_stamina_disrupt", ImpactSource::Shockwave, 36);
            }
        }
    }
    true
}

fn special_active_at(special: &ActiveSpecial, age_ms: u32) -> bool {
    special.active_window.contains_ms(age_ms)
}

fn special_active_progress(special: &ActiveSpecial) -> f32 {
    let age_ms = special.age.as_millis_floor();
    let start_ms = special.active_window.start_ms;
    let end_ms = special
        .active_window
        .end_ms
        .unwrap_or(special.total_lifetime_ms)
        .max(start_ms + 1);
    ((age_ms.saturating_sub(start_ms)) as f32 / (end_ms - start_ms) as f32).clamp(0.0, 1.0)
}

fn special_overlaps_target(
    special: &ActiveSpecial,
    radius: f32,
    origin: Vec3,
    target_position: &SimPosition,
) -> bool {
    let target = target_position.translation + Vec3::Y * (FIGHTER_HEIGHT * 0.58);
    let combined_radius = radius + FIGHTER_RADIUS;
    debug_assert!(combined_radius >= 0.0);
    match special.kind {
        SpecialKind::Projectile => {
            canonical_math::vec3_distance_squared(target, origin)
                <= combined_radius * combined_radius
        }
        _ => {
            flat_distance_squared(origin, target_position.translation)
                <= combined_radius * combined_radius
        }
    }
}

fn flat_distance_squared(a: Vec3, b: Vec3) -> f32 {
    canonical_math::vec2_distance_squared(Vec2::new(a.x, a.z), Vec2::new(b.x, b.z))
}

fn flat_distance(a: Vec3, b: Vec3) -> f32 {
    canonical_math::vec2_distance(Vec2::new(a.x, a.z), Vec2::new(b.x, b.z))
}

#[cfg(test)]
fn special_impact_profile(special: &ActiveSpecial, falloff: f32) -> ImpactProfile {
    special_impact_profile_from_payload(special, falloff, None)
}

fn special_impact_profile_with_feel(
    special: &ActiveSpecial,
    falloff: f32,
    feel: &CombatFeelTuning,
) -> ImpactProfile {
    special_impact_profile_from_payload(special, falloff, Some(feel))
}

fn special_impact_profile_from_payload(
    special: &ActiveSpecial,
    falloff: f32,
    feel: Option<&CombatFeelTuning>,
) -> ImpactProfile {
    let (damage_scale, knockback_scale) = match special.kind {
        SpecialKind::Shockwave => (falloff.max(0.45), falloff.max(0.55)),
        _ => (1.0, 1.0),
    };

    let mut profile = if let Some(feel) = feel {
        impact_profile_from_payload_with_feel(
            special.owner.index(),
            special.source,
            special.payload_id,
            damage_scale,
            knockback_scale,
            1.0,
            special.guard_stamina_damage,
            feel,
        )
    } else {
        impact_profile_from_payload(
            special.owner.index(),
            special.source,
            special.payload_id,
            damage_scale,
            knockback_scale,
            1.0,
            special.guard_stamina_damage,
        )
    };
    profile.shape_id = Some(special.shape_id);
    profile.attacker_style = Some(special.owner_style);
    profile
}

fn should_despawn_special(position: Vec3, arena: &ArenaDefinition) -> bool {
    debug_assert!(arena.ringout_radius >= 0.0);
    position.y < arena.ringout_y
        || canonical_math::vec2_length_squared(Vec2::new(position.x, position.z))
            > arena.ringout_radius * arena.ringout_radius
}

#[allow(dead_code)]
fn special_cost(kind: SpecialKind) -> f32 {
    special_definition(kind).cost
}

#[allow(dead_code)]
fn styled_special_cost(kind: SpecialKind, loadout: LoadoutContext) -> f32 {
    special_cost(kind) * loadout_special_cost_scale(loadout)
}

fn styled_special_cooldown(loadout: LoadoutContext) -> f32 {
    SPECIAL_COOLDOWN * loadout_special_cooldown_scale(loadout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::characters::{CharacterKind, CharacterMoveCatalog, FighterCharacter};
    use crate::combat::{begin_contact_collection, resolve_contacts};
    use crate::components::{FighterGrabState, FighterUltimateState};
    use crate::game_state::MatchTelemetry;
    use crate::reactions::ReactionFamilyId;
    use crate::sim_event::{PresentationEventCursor, PresentationEventRouter, SimEventJournal};

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct FrozenSpecialTargetState {
        fighter: FighterId,
        health_bits: u32,
        stamina_bits: u32,
        last_attacker: Option<FighterId>,
        action: FighterAction,
        reaction: Option<ReactionFamilyId>,
        velocity_bits: [u32; 3],
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct FrozenSpecialContactFixture {
        accepted_targets: Vec<FighterId>,
        events: Vec<SimEvent>,
        source: SimEntityId,
        source_hit_memory: u8,
        source_age_ticks: u32,
        source_lifetime_ticks: u32,
        source_active_feedback_sent: bool,
        target_state: Vec<FrozenSpecialTargetState>,
    }

    fn local_entity(index: u32) -> Entity {
        Entity::from_raw_u32(index).expect("fixture entity index should be valid")
    }

    fn special_capacities(capacity: u32) -> [u32; SimEntityKind::ALL.len()] {
        let mut capacities = [0; SimEntityKind::ALL.len()];
        capacities[SimEntityKind::Special.code() as usize] = capacity;
        capacities
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
                name: "special contact fixture",
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
            FighterEquipment::new(crate::equipment::EquipmentKind::CounterCell),
            SimPosition::new(position),
        )
    }

    fn spawn_contact_fixture_special(app: &mut App, position: Vec3) -> (Entity, SimEntityId) {
        let mut special = test_special(SpecialKind::Shockwave);
        // Collection advances once before testing the authored 70 ms active edge.
        special.age = ElapsedTicks::from_ticks(4);
        let entity = app
            .world_mut()
            .spawn((special, SimPosition::new(position)))
            .id();
        let stable = app
            .world_mut()
            .resource_mut::<SimulationIdentityAllocator>()
            .try_allocate(SimEntityKind::Special, entity)
            .unwrap();
        let source = stable.id();
        app.world_mut().entity_mut(entity).insert(stable);
        (entity, source)
    }

    fn run_frozen_special_contact_fixture(
        reverse_ecs_allocation: bool,
    ) -> FrozenSpecialContactFixture {
        let owner = FighterId::ZERO;
        let target_a = FighterId::new(1).unwrap();
        let target_b = FighterId::new(2).unwrap();
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
            .insert_resource(TickEventBuffer::new(SimTick(73)))
            .add_systems(
                Update,
                (
                    begin_contact_collection,
                    collect_special_contacts,
                    resolve_contacts,
                    apply_special_contact_outcomes,
                )
                    .chain(),
            );

        let early_source = (!reverse_ecs_allocation)
            .then(|| spawn_contact_fixture_special(&mut app, target_position));
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
            .unwrap_or_else(|| spawn_contact_fixture_special(&mut app, target_position));

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
        let special = app
            .world()
            .get::<ActiveSpecial>(source_entity)
            .expect("shockwave remains alive after its frozen multi-target batch");
        let (
            source_hit_memory,
            source_age_ticks,
            source_lifetime_ticks,
            source_active_feedback_sent,
        ) = (
            special.already_hit.bits(),
            special.age.get(),
            special.lifetime.remaining(),
            special.active_feedback_sent,
        );
        let target_state = {
            let world = app.world_mut();
            let mut fighters =
                world.query::<(&Fighter, &FighterStats, &FighterMotor, &FighterActionState)>();
            let mut state = fighters
                .iter(world)
                .filter_map(|(fighter, stats, motor, action)| {
                    let fighter = FighterId::from_index(fighter.id)?;
                    (fighter != owner).then_some(FrozenSpecialTargetState {
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

        FrozenSpecialContactFixture {
            accepted_targets,
            events,
            source,
            source_hit_memory,
            source_age_ticks,
            source_lifetime_ticks,
            source_active_feedback_sent,
            target_state,
        }
    }

    #[test]
    fn frozen_special_shockwave_is_independent_of_target_and_source_ecs_allocation_order() {
        let forward = run_frozen_special_contact_fixture(false);
        let reversed = run_frozen_special_contact_fixture(true);

        assert_eq!(forward, reversed);
        assert_eq!(
            forward.accepted_targets,
            vec![FighterId::new(1).unwrap(), FighterId::new(2).unwrap()]
        );
        assert_eq!(
            forward.source_hit_memory,
            (1 << FighterId::new(1).unwrap().index()) | (1 << FighterId::new(2).unwrap().index())
        );
        assert_eq!(forward.source_age_ticks, 5);
        assert!(forward.source_lifetime_ticks > 0);
        assert!(forward.source_active_feedback_sent);
        assert!(
            forward
                .target_state
                .iter()
                .all(|target| target.health_bits != MAX_HEALTH.to_bits()),
            "{forward:?}"
        );
        assert_eq!(forward.events.len(), 3);
        assert_eq!(
            forward.events[0],
            SimEvent {
                id: SimEventId {
                    tick: SimTick(73),
                    source: SimEventSource::Entity(forward.source),
                    ordinal: 0,
                },
                kind: SimEventKind::AbilityLifecycle {
                    entity: forward.source,
                    event: AbilityLifecycleEvent::Activated,
                },
            }
        );
        assert_eq!(
            forward
                .events
                .iter()
                .skip(1)
                .filter_map(|event| match event.kind {
                    SimEventKind::HitConfirmed { victim, .. } => Some(victim),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            vec![FighterId::new(1).unwrap(), FighterId::new(2).unwrap()]
        );
        assert_eq!(
            forward
                .events
                .iter()
                .map(|event| event.id.ordinal)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    fn presentation_intent_at(tick: u64, ordinal: u16) -> SpecialPresentationIntent {
        let entity = SimEntityId::new(SimEntityKind::Special, 0, 1);
        SpecialPresentationIntent {
            event_id: SimEventId {
                tick: SimTick(tick),
                source: SimEventSource::Entity(entity),
                ordinal,
            },
            entity,
            kind: SpecialPresentationKind::Lifecycle {
                event: AbilityLifecycleEvent::Spawned,
                position: Vec3::ZERO,
                direction: Vec3::Z,
                package: Some(FeedbackPackageId::SpecialProjectileStartup),
                cue: Some("special_projectile_prime"),
                source: ImpactSource::Projectile,
                priority: 20,
            },
        }
    }

    fn commit_presentation_event(
        journal: &mut SimEventJournal,
        intents: &mut SpecialPresentationIntentJournal,
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
            .record(SpecialPresentationIntent { event_id, ..intent })
            .unwrap();
        event_id
    }

    fn spawn_casting_fighter(app: &mut App, fighter_id: usize) -> Entity {
        app.world_mut()
            .spawn((
                Fighter {
                    id: fighter_id,
                    name: "Caster",
                    color: Color::WHITE,
                    spawn: Vec3::ZERO,
                },
                FighterInput {
                    special: true,
                    ..default()
                },
                FighterMotor {
                    facing: Vec3::Z,
                    ..default()
                },
                FighterActionState::default(),
                FighterSpecialState::default(),
                FighterStyle {
                    kind: FighterStyleKind::Anchor,
                },
                FighterEquipment {
                    kind: crate::equipment::EquipmentKind::CounterCell,
                    cooldown: TickTimer::ZERO,
                },
                SimPosition::default(),
            ))
            .id()
    }

    #[test]
    fn shockwave_radius_expands_over_lifetime() {
        let mut special = test_special(SpecialKind::Shockwave);
        special.age = ElapsedTicks::from_ticks(
            TickTimer::from_seconds_ceil(SPECIAL_SHOCKWAVE_LIFETIME * 0.5).remaining(),
        );
        let mid = active_special_radius(&special);
        special.age = ElapsedTicks::from_ticks(
            TickTimer::from_seconds_ceil(SPECIAL_SHOCKWAVE_LIFETIME).remaining(),
        );
        assert!(active_special_radius(&special) > mid);
    }

    #[test]
    fn special_profiles_use_distinct_sources() {
        let projectile = special_impact_profile(&test_special(SpecialKind::Projectile), 1.0);
        let trap = special_impact_profile(&test_special(SpecialKind::Trap), 1.0);

        assert_eq!(projectile.source, ImpactSource::Projectile);
        assert_eq!(
            projectile.payload_id,
            Some(AttackPayloadId::SpecialProjectile)
        );
        assert_eq!(trap.source, ImpactSource::Trap);
        assert_eq!(trap.payload_id, Some(AttackPayloadId::SpecialTrap));
        assert_eq!(trap.shape_id, Some(AttackShapeId::TrapPlate));
    }

    #[test]
    fn specials_use_authored_activation_windows() {
        let projectile = test_special(SpecialKind::Projectile);
        assert_eq!(projectile.launch_ms, 90);
        assert!(!special_active_at(&projectile, 119));
        assert!(special_active_at(&projectile, 120));

        let trap = test_special(SpecialKind::Trap);
        assert!(!special_active_at(&trap, 259));
        assert!(special_active_at(&trap, 260));

        let shockwave = test_special(SpecialKind::Shockwave);
        assert!(special_active_at(&shockwave, 70));
        assert!(!special_active_at(&shockwave, 301));
    }

    #[test]
    fn repeating_specials_clear_contact_memory_on_authored_ticks() {
        let mut hazard = test_special(SpecialKind::Hazard);
        hazard
            .already_hit
            .insert(FighterId::new(2).expect("valid fighter"));
        let repeated = advance_special_repeat_window(&mut hazard, 759);
        assert!(!repeated);
        assert_eq!(hazard.already_hit.len(), 1);

        let repeated = advance_special_repeat_window(&mut hazard, 760);
        assert!(hazard.already_hit.is_empty());
        assert!(repeated);
    }

    #[test]
    fn specials_carry_authored_presentation_packages() {
        let projectile = special_definition(SpecialKind::Projectile).presentation;
        let shockwave = special_definition(SpecialKind::Shockwave).presentation;
        let hazard = special_definition(SpecialKind::Hazard).presentation;

        assert_eq!(
            projectile.impact_package,
            FeedbackPackageId::SpecialProjectileImpact
        );
        assert_eq!(
            shockwave.active_package,
            FeedbackPackageId::SpecialShockwaveRelease
        );
        assert_eq!(
            hazard.repeat_package,
            Some(FeedbackPackageId::SpecialHazardPulse)
        );
    }

    #[test]
    fn special_presentation_journal_is_bounded_and_rejects_bad_ordinals() {
        let mut intents = SpecialPresentationIntentJournal::default();
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
    fn special_events_survive_render_stall_and_rollback_exactly_once() {
        let mut journal = SimEventJournal::default();
        let mut intents = SpecialPresentationIntentJournal::default();
        for tick in 20..23 {
            commit_presentation_event(&mut journal, &mut intents, tick);
        }

        let mut cursor = PresentationEventCursor::default();
        let mut router = PresentationEventRouter::default();
        let mut presented = Vec::new();
        cursor
            .route_available(&journal, &mut router, Some(SimTick(22)), |event| {
                if let Some(intent) = intents.get(event.id)
                    && special_presentation_matches_event(event, intent)
                {
                    presented.push(event.id);
                }
            })
            .unwrap();
        assert_eq!(presented.len(), 3);

        let retained = SimTick(20);
        journal.discard_after(retained);
        cursor.discard_after(retained);
        router.discard_after(retained);
        intents.discard_after(retained);
        for tick in 21..23 {
            commit_presentation_event(&mut journal, &mut intents, tick);
        }
        cursor
            .route_available(&journal, &mut router, Some(SimTick(22)), |event| {
                if let Some(intent) = intents.get(event.id)
                    && special_presentation_matches_event(event, intent)
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
    fn catalyst_style_cycles_specials_faster() {
        let catalyst = LoadoutContext::new(
            crate::styles::FighterStyleKind::Catalyst,
            crate::equipment::EquipmentKind::CounterCell,
        );
        let anchor = LoadoutContext::new(
            crate::styles::FighterStyleKind::Anchor,
            crate::equipment::EquipmentKind::CounterCell,
        );
        assert!(
            styled_special_cost(SpecialKind::Projectile, catalyst)
                < styled_special_cost(SpecialKind::Projectile, anchor)
        );
        assert!(styled_special_cooldown(catalyst) < styled_special_cooldown(anchor));
    }

    #[test]
    fn catalyst_specials_apply_disruption_without_damage_bonus() {
        let mut special = test_special(SpecialKind::Projectile);
        special.stamina_disrupt = loadout_special_stamina_disrupt(LoadoutContext::new(
            crate::styles::FighterStyleKind::Catalyst,
            crate::equipment::EquipmentKind::CounterCell,
        ));
        let profile = special_impact_profile(&special, 1.0);

        assert!(special.stamina_disrupt > 0.0);
        assert_eq!(profile.damage, SPECIAL_PROJECTILE_DAMAGE);
    }

    #[test]
    fn special_casts_allocate_stable_ids_in_fighter_slot_order() {
        let mut app = App::new();
        app.insert_resource(SimulationIdentityAllocator::with_capacities(
            special_capacities(2),
        ))
        .insert_resource(Hitstop::default())
        .insert_resource(ActiveArena::default())
        .insert_resource(TickEventBuffer::new(SimTick(4)))
        .add_systems(Update, handle_special_inputs);

        let fighter_one = spawn_casting_fighter(&mut app, 1);
        let fighter_zero = spawn_casting_fighter(&mut app, 0);
        assert!(fighter_one.index() < fighter_zero.index());

        app.update();

        let identities = app.world().resource::<SimulationIdentityAllocator>();
        let (_, first_entity) = special_pool_entry(&identities, 0).unwrap();
        let (_, second_entity) = special_pool_entry(&identities, 1).unwrap();
        assert_eq!(
            app.world()
                .get::<ActiveSpecial>(first_entity)
                .unwrap()
                .owner,
            FighterId::ZERO
        );
        assert_eq!(
            app.world()
                .get::<ActiveSpecial>(second_entity)
                .unwrap()
                .owner,
            FighterId::from_index(1).unwrap()
        );
        assert!(app.world().get::<StableSimEntity>(first_entity).is_some());
        assert!(app.world().get::<StableSimEntity>(second_entity).is_some());
        assert!(app.world().get::<SimPosition>(first_entity).is_some());
        assert!(app.world().get::<Transform>(first_entity).is_none());
        assert!(app.world().get::<Mesh3d>(first_entity).is_none());
        assert!(
            app.world()
                .get::<MeshMaterial3d<StandardMaterial>>(first_entity)
                .is_none()
        );
        assert!(app.world().get::<SpecialVisualRoot>(first_entity).is_none());
        assert!(app.world().get_resource::<EffectAssets>().is_none());
        assert!(app.world().get_resource::<HitEffects>().is_none());
        assert_eq!(app.world().resource::<TickEventBuffer>().len(), 2);
    }

    #[test]
    fn special_identity_pool_rejects_overflow_without_evicting_live_special() {
        let mut identities = SimulationIdentityAllocator::with_capacities(special_capacities(1));
        let live = identities
            .try_allocate(SimEntityKind::Special, local_entity(7))
            .unwrap();

        let overflow = identities
            .try_allocate(SimEntityKind::Special, local_entity(8))
            .unwrap_err();

        assert_eq!(live.id().kind(), SimEntityKind::Special);
        assert_eq!(overflow.kind, SimEntityKind::Special);
        assert_eq!(identities.mapped_entity(live.id()), Some(local_entity(7)));
        assert_eq!(identities.rejected_spawns(SimEntityKind::Special), 1);
    }

    #[test]
    fn stale_special_release_cannot_remove_reused_generation() {
        let mut identities = SimulationIdentityAllocator::with_capacities(special_capacities(1));
        let old_entity = local_entity(4);
        let old = identities
            .try_allocate(SimEntityKind::Special, old_entity)
            .unwrap();
        assert!(identities.release(old_entity, old));

        let replacement_entity = local_entity(5);
        let replacement = identities
            .try_allocate(SimEntityKind::Special, replacement_entity)
            .unwrap();
        assert_eq!(old.id().index(), replacement.id().index());
        assert_ne!(old.id().generation(), replacement.id().generation());

        assert!(!identities.release(old_entity, old));
        assert_eq!(
            identities.mapped_entity(replacement.id()),
            Some(replacement_entity)
        );
        assert_eq!(identities.live_count(SimEntityKind::Special), 1);
    }

    #[test]
    fn special_update_order_follows_stable_slots_not_local_entity_indices() {
        let mut identities = SimulationIdentityAllocator::with_capacities(special_capacities(2));
        let later_local_entity = local_entity(19);
        let earlier_local_entity = local_entity(3);
        let stable_first = identities
            .try_allocate(SimEntityKind::Special, later_local_entity)
            .unwrap();
        let stable_second = identities
            .try_allocate(SimEntityKind::Special, earlier_local_entity)
            .unwrap();

        let order: Vec<_> = (0..identities.capacity(SimEntityKind::Special))
            .filter_map(|index| special_pool_entry(&identities, index))
            .collect();

        assert_eq!(
            order,
            vec![
                (stable_first.id(), later_local_entity),
                (stable_second.id(), earlier_local_entity),
            ]
        );
        assert!(order[0].1.index() > order[1].1.index());
    }

    #[test]
    fn expired_special_releases_identity_in_same_update() {
        let mut app = App::new();
        app.insert_resource(SimulationIdentityAllocator::with_capacities(
            special_capacities(1),
        ))
        .insert_resource(MatchState::default())
        .insert_resource(ActiveArena::default())
        .insert_resource(CombatFeelTuning::default())
        .insert_resource(Hitstop::default())
        .insert_resource(TickEventBuffer::new(SimTick(8)))
        .init_resource::<ContactBuffer>()
        .add_systems(
            Update,
            (collect_special_contacts, apply_special_contact_outcomes).chain(),
        );

        let mut special = test_special(SpecialKind::Projectile);
        special.lifetime = TickTimer::from_ticks(1);
        let entity = app
            .world_mut()
            .spawn((special, SimPosition::default()))
            .id();
        let stable = app
            .world_mut()
            .resource_mut::<SimulationIdentityAllocator>()
            .try_allocate(SimEntityKind::Special, entity)
            .unwrap();
        app.world_mut().entity_mut(entity).insert(stable);

        app.update();

        assert!(app.world().get_entity(entity).is_err());
        assert_eq!(
            app.world()
                .resource::<SimulationIdentityAllocator>()
                .live_count(SimEntityKind::Special),
            0
        );
    }

    fn test_special(kind: SpecialKind) -> ActiveSpecial {
        ActiveSpecial {
            kind,
            owner: FighterId::ZERO,
            owner_style: FighterStyleKind::Anchor,
            payload_id: special_definition(kind).payload_id,
            shape_id: special_definition(kind).shape_id,
            source: special_definition(kind).source,
            facing: Vec3::Z,
            velocity: Vec3::ZERO,
            lifetime: TickTimer::from_seconds_ceil(1.0),
            age: ElapsedTicks::ZERO,
            total_lifetime_ms: 1000,
            radius: 1.0,
            grace: TickTimer::ZERO,
            launch_ms: special_definition(kind).timing.launch_ms,
            active_window: special_definition(kind).timing.active_window,
            repeat_ms: special_definition(kind).timing.repeat_ms,
            next_repeat_ms: special_definition(kind).timing.repeat_ms.map(|repeat_ms| {
                special_definition(kind).timing.active_window.start_ms + repeat_ms
            }),
            active_feedback_sent: false,
            aftermath_ms: special_definition(kind).timing.aftermath_ms,
            aftermath_feedback_sent: false,
            active_cue: special_definition(kind).timing.active_cue,
            aftermath_cue: special_definition(kind).timing.aftermath_cue,
            active_package: special_definition(kind).presentation.active_package,
            repeat_package: special_definition(kind).presentation.repeat_package,
            impact_package: special_definition(kind).presentation.impact_package,
            aftermath_package: special_definition(kind).presentation.aftermath_package,
            despawn_package: special_definition(kind).presentation.despawn_package,
            stamina_disrupt: 0.0,
            guard_stamina_damage: special_definition(kind).guard_stamina_damage,
            already_hit: FighterHitMask::default(),
        }
    }
}
