use arrayvec::ArrayVec;
use bevy::camera::visibility::RenderLayers;
use bevy::gltf::GltfAssetLabel;
use bevy::math::EulerRot;
use bevy::prelude::*;
use bevy::scene::SceneInstanceReady;
use serde::Deserialize;
use std::collections::HashMap;
use std::f32::consts::{PI, TAU};
#[cfg(all(
    feature = "dev-hot-reload",
    not(feature = "shipping"),
    not(target_arch = "wasm32")
))]
use std::fs;
use std::mem::size_of;
use std::path::Path;
use std::sync::OnceLock;

use crate::arena_barriers::ArenaBarrierDefinition;
use crate::arena_defs::{
    ActiveArena, ArenaBackgroundDefinition, ArenaDefinition, ArenaGroundShape,
    ArenaHazardDefinition, ArenaHazardKind, ArenaPipePairDefinition, ArenaVisualTheme,
    CRANK_PIPE_VISUAL_SCALE, CRANK_YARD_ARENA_INDEX, PlatformDefinition, arena_definitions,
};
use crate::arena_prop_colliders::{
    LocalPropBarrier, PropBarrierBehavior, WorldPropBarrier, prop_collision_profile,
};
use crate::camera::ArenaCamera;
use crate::combat::{
    CombatPresentationIntentJournal, ImpactFeedbackIntensity, ImpactOutcome, ImpactProfile,
    ImpactSource, NEUTRAL_IMPACT_OWNER_ID, can_receive_impact, impact_profile,
};
use crate::components::{
    Fighter, FighterAction, FighterActionState, FighterInput, FighterMotor, FighterStats,
    SimPosition,
};
#[cfg(test)]
use crate::constants::ARENA_RADIUS;
use crate::constants::{
    ARENA_HEIGHT, ARENA_TOP_Y, FIGHTER_COUNT, FIGHTER_RADIUS, GRAVITY, LEDGE_SUPPORT_GRACE_MAX,
    LEDGE_SUPPORT_GRACE_SCALE,
};
use crate::contact_arbitration::{
    ContactBuffer, ContactFlags, ContactOutcomeKind, ContactPhase, ContactRecord, ContactSourceId,
    ContactSourceKind,
};
use crate::determinism::{
    DEFAULT_F32_QUANTIZATION, FighterId, SimEntityId, SimEntityKind, dequantize_f32, quantize_f32,
};
use crate::ecs_identity::{
    SIM_ENTITY_POOL_CAPACITIES, StableEntityCommands, StableSimEntity, despawn_stable,
    try_spawn_stable,
};
use crate::effects::{EffectAssets, spawn_burning_fighter_effect, spawn_machine_scratch};
use crate::feel::CombatFeelTuning;
use crate::game_state::{Hitstop, MatchState};
use crate::reactions::ReactionFamilyId;
use crate::rollback::RollbackEventDiscard;
use crate::sim_event::{
    EventEmitError, MAX_SIM_EVENTS_PER_TICK, SIM_EVENT_HISTORY_TICKS, SimEventId,
};
use crate::simulation::{
    ElapsedTicks, SIM_DT_SECONDS, SimTick, TickTimer, milliseconds_to_ticks_ceil,
    seconds_to_ticks_ceil,
};
use crate::snapshot::{
    ARENA_PAYLOAD_BYTES, ArenaRuntimeSnapshot, FighterPipeSnapshot, QuantizedVec3,
};
use crate::techniques::DamageElement;

const ARENA_HAZARD_PULSE_DAMAGE: f32 = 7.0;
const ARENA_HAZARD_PULSE_KNOCKBACK: f32 = 5.8;
const ARENA_HAZARD_SNARE_DAMAGE: f32 = 3.0;
const ARENA_HAZARD_SNARE_KNOCKBACK: f32 = 2.2;
const ARENA_HAZARD_BUMPER_DAMAGE: f32 = 9.0;
const ARENA_HAZARD_BUMPER_KNOCKBACK: f32 = 7.6;
const ARENA_HAZARD_CAMPFIRE_DAMAGE: f32 = 4.0;
const ARENA_HAZARD_CAMPFIRE_KNOCKBACK: f32 = 8.8;
const ARENA_HAZARD_CAMPFIRE_LAUNCH: f32 = 4.6;
const ARENA_HAZARD_CAMPFIRE_BURN_SECONDS: f32 = 1.35;
const ARENA_HAZARD_SAW_DAMAGE: f32 = 5.0;
const ARENA_HAZARD_SAW_KNOCKBACK: f32 = 12.0;
const ARENA_HAZARD_SAW_LAUNCH: f32 = 4.8;
const PIPE_ENTER_SECONDS: f32 = 0.32;
const PIPE_TRAVEL_SECONDS: f32 = 0.12;
const PIPE_EXIT_SECONDS: f32 = 0.34;
const PIPE_ENTRY_DWELL_TICKS: u32 = milliseconds_to_ticks_ceil(250);
const PIPE_ENTER_END_TICKS: u32 = milliseconds_to_ticks_ceil(320);
const PIPE_HIDDEN_END_TICKS: u32 = milliseconds_to_ticks_ceil(440);
const PIPE_TRANSIT_END_TICKS: u32 = milliseconds_to_ticks_ceil(780);
const PIPE_SINK_DEPTH: f32 = 1.15;
const PIPE_EXIT_CLEARANCE_RADIUS: f32 = 1.05;
const PIPE_EXIT_HOP_SPEED: f32 = 3.2;
const PIPE_EXIT_INWARD_SPEED: f32 = 2.4;
const MINI_ARENA_ASSET_ROOT: &str = "arena/kenney_mini_arena";
const ARENA_KIT_ASSET_ROOT: &str = "arena/kits";
const MINI_ARENA_FLOOR_SPACING: f32 = 1.6;
const MINI_ARENA_FLOOR_SCALE: f32 = 1.62;
const CHAMPIONS_COURT_ARENA_INDEX: usize = 0;
const VENT_SPIRAL_ARENA_INDEX: usize = 4;
const POWDER_KEG_ARENA_INDEX: usize = 9;
const CRANK_SAW_VISUAL_Y: f32 = ARENA_TOP_Y + 0.72;
const CRANK_LEVER_POSITION: Vec3 = Vec3::new(6.7, ARENA_TOP_Y, 1.7);
const CRANK_LEVER_ATTACK_RADIUS: f32 = 1.85;
const POWDER_CANNON_BOMB_DAMAGE: f32 = 9.0;
const POWDER_CANNON_BOMB_RADIUS: f32 = 1.05;
const CHAMPIONS_COURT_RON_PATH: &str = "arts/champions_court.ron";
#[cfg(not(all(
    feature = "dev-hot-reload",
    not(feature = "shipping"),
    not(target_arch = "wasm32")
)))]
const EMBEDDED_CHAMPIONS_COURT_RON: &str = include_str!("../arts/champions_court.ron");
const CHAMPIONS_COURT_LIGHT_SCALE: f32 = 1_000.0;
const CHAMPIONS_COURT_MAP_LIGHTS_ENABLED: bool = false;
const PLATFORM_SIDE_COLLISION_MIN_TOP_Y: f32 = ARENA_TOP_Y + 0.08;
const ARENA_GROUND_DEPTH_BIAS_BASE: f32 = -2_048.0;
const ARENA_GROUND_DEPTH_BIAS_STEP: f32 = 128.0;
const ARENA_PLATFORM_DEPTH_BIAS_BASE: f32 = -768.0;
const ARENA_PLATFORM_DEPTH_BIAS_STEP: f32 = 64.0;
const ARENA_PROP_SURFACE_CLEARANCE: f32 = 0.012;
pub(crate) const ARENA_PREVIEW_RENDER_LAYER: usize = 21;

#[derive(Component)]
pub struct ArenaGeometry;

/// One non-rendering root marker is spawned for each arena-visual generation.
/// It gives graphical performance fixtures an exact scene identity to await;
/// regular arena teardown removes it alongside the other geometry roots.
#[derive(Component)]
pub(crate) struct ArenaSceneReadyMarker {
    arena_index: usize,
    generation: u64,
}

impl ArenaSceneReadyMarker {
    pub(crate) const fn arena_index(&self) -> usize {
        self.arena_index
    }

    pub(crate) const fn generation(&self) -> u64 {
        self.generation
    }
}

/// Copies the canonical match arena into the per-world lookup resource at the
/// tick boundary. Every authoritative system in a step therefore observes one
/// immutable selection, and separate authority worlds cannot affect each other.
pub fn sync_active_arena_from_match_state(
    state: Res<MatchState>,
    mut active_arena: ResMut<ActiveArena>,
) {
    if active_arena.index() != state.arena_index {
        active_arena.select(state.arena_index);
    }
}

fn arena_geometry_render_layers() -> RenderLayers {
    RenderLayers::from_layers(&[0, ARENA_PREVIEW_RENDER_LAYER])
}

fn apply_arena_geometry_render_layers(
    scene_ready: On<SceneInstanceReady>,
    children: Query<&Children>,
    mut commands: Commands,
) {
    commands
        .entity(scene_ready.entity)
        .insert(arena_geometry_render_layers());
    for descendant in children.iter_descendants(scene_ready.entity) {
        commands
            .entity(descendant)
            .insert(arena_geometry_render_layers());
    }
}

pub fn sync_arena_preview_render_layers(
    mut commands: Commands,
    children: Query<&Children>,
    geometry: Query<Entity, (Added<ArenaGeometry>, Without<ArenaBackgroundWallpaper>)>,
) {
    for entity in &geometry {
        commands
            .entity(entity)
            .insert(arena_geometry_render_layers())
            .observe(apply_arena_geometry_render_layers);
        for descendant in children.iter_descendants(entity) {
            commands
                .entity(descendant)
                .insert(arena_geometry_render_layers());
        }
    }
}

#[derive(Component)]
pub(crate) struct ArenaBackgroundWallpaper(ArenaBackgroundDefinition);

#[derive(Component)]
pub struct ArenaHazardMarker {
    kind: ArenaHazardKind,
    pulse_seconds: f32,
    phase: f32,
    base_scale: f32,
    base_y: f32,
}

#[derive(Component)]
pub struct ArenaCampfireFlame {
    base_scale: Vec3,
    phase: f32,
}

#[derive(Component)]
pub struct ArenaPipePortalRing {
    endpoint: usize,
    phase: f32,
    base_scale: Vec3,
}

#[derive(Component)]
pub struct ArenaPipePortalParticle {
    endpoint: usize,
    phase: f32,
    radius: f32,
    base_y: f32,
}

#[derive(Component)]
pub struct ArenaSawBladeVisual {
    spin_speed: f32,
}

#[derive(Component)]
pub struct ArenaSawWarningLight {
    phase: f32,
    base_scale: Vec3,
}

#[derive(Component)]
pub struct ArenaSawAmbientSpark {
    center: Vec3,
    phase: f32,
}

#[derive(Component)]
pub(crate) struct CrankLeverVisual {
    running_rotation: Quat,
    stopped_rotation: Quat,
}

#[derive(Component)]
pub(crate) struct ArenaCannonBomb {
    pub(crate) velocity: Vec3,
    pub(crate) lifetime: TickTimer,
}

const ARENA_ORDNANCE_ENTITY_CAPACITY: usize =
    SIM_ENTITY_POOL_CAPACITIES[SimEntityKind::ArenaOrdnance.code() as usize] as usize;

/// Fixed handoff for cannon sources whose frozen geometry requires a
/// post-resolution despawn. It is rebuilt before every advancing cannon tick
/// and intentionally does not participate in snapshots.
#[derive(Resource, Default)]
pub struct ArenaOrdnanceContactFrame {
    tick: Option<SimTick>,
    detonations: [Option<SimEntityId>; ARENA_ORDNANCE_ENTITY_CAPACITY],
    detonation_len: usize,
}

impl ArenaOrdnanceContactFrame {
    fn begin_tick(&mut self, tick: SimTick) {
        for source in &mut self.detonations[..self.detonation_len] {
            *source = None;
        }
        self.tick = Some(tick);
        self.detonation_len = 0;
    }

    fn cancel_tick(&mut self) {
        self.tick = None;
        self.detonation_len = 0;
    }

    fn record_detonation(&mut self, source: SimEntityId) {
        if self.detonations[..self.detonation_len].contains(&Some(source))
            || self.detonation_len == self.detonations.len()
        {
            return;
        }
        self.detonations[self.detonation_len] = Some(source);
        self.detonation_len += 1;
    }

    fn detonations(&self) -> impl Iterator<Item = SimEntityId> + '_ {
        self.detonations[..self.detonation_len]
            .iter()
            .flatten()
            .copied()
    }
}

/// Render-only child of an authoritative cannon bomb. Keeping mesh rotation on
/// this child prevents render-rate animation from mutating canonical transforms.
#[derive(Component)]
pub(crate) struct ArenaCannonBombVisual;

#[derive(Component)]
pub struct ArenaVentRotor {
    pulse_seconds: f32,
    phase: f32,
    spin_direction: f32,
}

#[derive(Component)]
pub struct ArenaVentWarning {
    pulse_seconds: f32,
    phase: f32,
    base_scale: Vec3,
}

#[derive(Component)]
pub struct ArenaVentPlume {
    pulse_seconds: f32,
    phase: f32,
    base_y: f32,
    full_height: f32,
    base_scale: Vec3,
}

#[derive(Component)]
pub struct ArenaVentUfo {
    base_y: f32,
}

#[derive(Component)]
pub struct ArenaVentUfoBeam {
    base_y: f32,
    base_scale: Vec3,
}

#[derive(Component, Clone, Copy, Debug)]
pub struct ArenaFighterBurn {
    remaining_seconds: f32,
    duration_seconds: f32,
}

impl ArenaFighterBurn {
    fn new(duration: f32) -> Self {
        Self {
            remaining_seconds: duration,
            duration_seconds: duration,
        }
    }

    pub fn visual_amount(self) -> f32 {
        let fade = (self.remaining_seconds / self.duration_seconds.max(0.01)).clamp(0.0, 1.0);
        let flicker = 0.76 + (self.remaining_seconds * 19.0).sin().abs() * 0.24;
        fade.sqrt() * flicker
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum FighterPipeState {
    Ready {
        candidate: Option<usize>,
        dwell_ticks: u32,
        cooldown: TickTimer,
    },
    Transit {
        source: usize,
        destination: usize,
        elapsed: ElapsedTicks,
        entry_y: f32,
        base_scale: Vec3,
    },
}

impl Default for FighterPipeState {
    fn default() -> Self {
        Self::Ready {
            candidate: None,
            dwell_ticks: 0,
            cooldown: TickTimer::ZERO,
        }
    }
}

#[derive(Resource)]
pub struct ArenaPipeState {
    arena_index: usize,
    fighters: [FighterPipeState; FIGHTER_COUNT],
}

impl ArenaPipeState {
    pub(crate) fn new(arena_index: usize) -> Self {
        Self {
            arena_index,
            fighters: [FighterPipeState::default(); FIGHTER_COUNT],
        }
    }

    fn sync_to_arena(&mut self, arena_index: usize) {
        if self.arena_index != arena_index {
            *self = Self::new(arena_index);
        }
    }

    fn endpoint_active(&self, endpoint: usize) -> bool {
        self.fighters.iter().any(|state| {
            matches!(
                state,
                FighterPipeState::Transit {
                    source,
                    destination,
                    ..
                } if *source == endpoint || *destination == endpoint
            )
        })
    }

    /// Render-only root scale for an active canonical pipe transit.
    /// Position and completion remain fixed-step state; callers may project
    /// this value into a Bevy `Transform` without feeding it back.
    pub(crate) fn fighter_transit_visual_scale(
        &self,
        fighter: FighterId,
        pipe_pair: Option<ArenaPipePairDefinition>,
    ) -> Option<Vec3> {
        let pipe_pair = pipe_pair?;
        let FighterPipeState::Transit {
            source,
            destination,
            elapsed,
            entry_y,
            base_scale,
        } = self.fighters[fighter.index()]
        else {
            return None;
        };
        Some(
            pipe_transit_sample(pipe_pair, source, destination, elapsed, entry_y, base_scale).scale,
        )
    }
}

#[allow(dead_code)]
#[derive(Clone)]
struct ChampionsCourtFloorRenderAsset {
    mesh: Handle<Mesh>,
    material: Handle<StandardMaterial>,
}

#[derive(Clone, Copy)]
struct ArenaAssetProp {
    name: &'static str,
    file: &'static str,
    x: f32,
    y: f32,
    z: f32,
    yaw: f32,
    scale: f32,
}

#[derive(Clone, Copy)]
struct ArenaThemePalette {
    primary: Color,
    secondary: Color,
    trim: Color,
    hazard: Color,
}

impl ArenaAssetProp {
    fn transform(self) -> Transform {
        Transform::from_xyz(self.x, self.y + ARENA_PROP_SURFACE_CLEARANCE, self.z)
            .with_rotation(Quat::from_rotation_y(self.yaw))
            .with_scale(Vec3::splat(self.scale))
    }

    fn collision_barriers(self) -> impl Iterator<Item = WorldPropBarrier> {
        prop_collision_profile(self.file)
            .iter()
            .copied()
            .map(move |barrier: LocalPropBarrier| {
                barrier.to_world(
                    Vec3::new(self.x, self.y + ARENA_PROP_SURFACE_CLEARANCE, self.z),
                    self.yaw,
                    self.scale,
                )
            })
    }
}

#[derive(Debug, Deserialize)]
struct ChampionsCourtRon {
    map: ChampionsCourtMap,
    assets: HashMap<String, String>,
    floor_shapes: Vec<ChampionsCourtFloorShape>,
    #[serde(default)]
    prefabs: HashMap<String, Vec<ChampionsCourtObject>>,
    #[serde(default)]
    instances: Vec<ChampionsCourtObject>,
    #[serde(default)]
    prefab_instances: Vec<ChampionsCourtPrefabInstance>,
    #[serde(default)]
    lights: Vec<ChampionsCourtLight>,
}

#[derive(Debug, Deserialize)]
struct ChampionsCourtMap {
    tile_size: f32,
}

#[derive(Debug, Deserialize)]
struct ChampionsCourtFloorShape {
    id: String,
    kind: String,
    asset: String,
    center: (i32, i32),
    #[serde(default)]
    radius_tiles: i32,
    #[serde(default)]
    inner_radius_tiles: i32,
    #[serde(default)]
    outer_radius_tiles: i32,
    #[serde(default)]
    size_tiles: (i32, i32),
    #[serde(default)]
    y: f32,
    #[serde(default)]
    rotation_y: f32,
}

#[derive(Clone, Debug, Deserialize)]
struct ChampionsCourtObject {
    #[serde(default)]
    id: String,
    asset: String,
    position: (f32, f32, f32),
    #[serde(default)]
    rotation_y: f32,
    #[serde(default = "unit_tuple3")]
    scale: (f32, f32, f32),
}

#[derive(Debug, Deserialize)]
struct ChampionsCourtPrefabInstance {
    id: String,
    prefab: String,
    position: (f32, f32, f32),
    #[serde(default)]
    rotation_y: f32,
    #[serde(default = "unit_tuple3")]
    scale: (f32, f32, f32),
}

#[derive(Debug, Deserialize)]
struct ChampionsCourtLight {
    id: String,
    kind: String,
    #[serde(default)]
    position: (f32, f32, f32),
    #[serde(default)]
    rotation_euler_degrees: (f32, f32, f32),
    #[serde(default = "white_tuple3")]
    color: (f32, f32, f32),
    #[serde(default)]
    intensity: f32,
    #[serde(default)]
    illuminance: f32,
    #[serde(default)]
    range: f32,
    #[serde(default)]
    shadows: bool,
}

#[derive(Resource)]
pub struct ArenaScene {
    index: usize,
    generation: u64,
}

impl ArenaScene {
    const INITIAL_GENERATION: u64 = 1;

    fn new(index: usize) -> Self {
        Self {
            index,
            generation: Self::INITIAL_GENERATION,
        }
    }

    pub(crate) const fn index(&self) -> usize {
        self.index
    }

    pub(crate) const fn generation(&self) -> u64 {
        self.generation
    }

    fn rebuild(&mut self, index: usize) -> u64 {
        let generation = self
            .generation
            .checked_add(1)
            .expect("arena render generation exhausted");
        self.index = index;
        self.generation = generation;
        self.generation
    }
}

#[derive(Resource)]
pub struct ArenaHazardState {
    arena_index: usize,
    elapsed: ElapsedTicks,
    hit_cooldowns: Vec<[TickTimer; FIGHTER_COUNT]>,
    crank_saws_stopped: bool,
    crank_lever_toggle_cooldown: TickTimer,
}

impl ArenaHazardState {
    pub(crate) fn new(arena_index: usize, hazard_count: usize) -> Self {
        Self {
            arena_index,
            elapsed: ElapsedTicks::ZERO,
            hit_cooldowns: vec![[TickTimer::ZERO; FIGHTER_COUNT]; hazard_count],
            crank_saws_stopped: false,
            crank_lever_toggle_cooldown: TickTimer::ZERO,
        }
    }

    fn sync_to_arena(&mut self, arena_index: usize, hazard_count: usize) {
        if self.arena_index == arena_index && self.hit_cooldowns.len() == hazard_count {
            return;
        }

        *self = Self::new(arena_index, hazard_count);
    }

    fn tick_cooldowns(&mut self) {
        for hazard_cooldowns in &mut self.hit_cooldowns {
            for cooldown in hazard_cooldowns {
                cooldown.tick();
            }
        }
    }

    pub fn elapsed(&self) -> f32 {
        self.elapsed.as_seconds()
    }

    pub fn elapsed_ticks(&self) -> ElapsedTicks {
        self.elapsed
    }
}

#[derive(Resource, Default)]
pub(crate) struct ArenaOrdnanceAssets {
    bomb_mesh: Handle<Mesh>,
    bomb_material: Handle<StandardMaterial>,
}

/// Arena-only visual punctuation paired with an authoritative impact event.
///
/// The impact itself is represented by the compact semantic `HitConfirmed` or
/// `Guarded` event. This sidecar carries only renderer-facing data that is not
/// allowed into snapshots or state hashes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ArenaImpactAccent {
    None,
    CampfireBurn { duration_seconds: f32 },
    MachineScratch { position: Vec3 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ArenaPresentationIntent {
    pub event_id: SimEventId,
    pub victim: FighterId,
    pub outcome: ImpactOutcome,
    pub accent: ArenaImpactAccent,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ArenaPresentationIntentSlot {
    tick: SimTick,
    len: u16,
    occupied: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ArenaPresentationIntentMetrics {
    pub recorded: u64,
    pub replaced: u64,
    pub rejected: u64,
    pub discarded: u64,
}

/// Fixed-capacity rollback sidecar for arena impact presentation.
///
/// Storage is allocated once, indexed by canonical tick and event ordinal, and
/// rejects invalid ordinals. A render stall therefore cannot grow memory, and
/// correction can discard speculative intents without touching simulation.
#[derive(Resource, Clone, Debug)]
pub struct ArenaPresentationIntentJournal {
    slots: [ArenaPresentationIntentSlot; SIM_EVENT_HISTORY_TICKS],
    intents: Box<[Option<ArenaPresentationIntent>]>,
    len: usize,
    metrics: ArenaPresentationIntentMetrics,
}

impl Default for ArenaPresentationIntentJournal {
    fn default() -> Self {
        Self {
            slots: [ArenaPresentationIntentSlot::default(); SIM_EVENT_HISTORY_TICKS],
            intents: vec![None; SIM_EVENT_HISTORY_TICKS * MAX_SIM_EVENTS_PER_TICK]
                .into_boxed_slice(),
            len: 0,
            metrics: ArenaPresentationIntentMetrics::default(),
        }
    }
}

impl ArenaPresentationIntentJournal {
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
    pub const fn metrics(&self) -> ArenaPresentationIntentMetrics {
        self.metrics
    }

    pub(crate) fn record(&mut self, intent: ArenaPresentationIntent) -> Result<(), EventEmitError> {
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
            *slot = ArenaPresentationIntentSlot {
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

    pub(crate) fn get(&self, event_id: SimEventId) -> Option<ArenaPresentationIntent> {
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
            self.slots[slot_index] = ArenaPresentationIntentSlot::default();
            self.len = self.len.saturating_sub(usize::from(slot.len));
            self.metrics.discarded = self.metrics.discarded.saturating_add(u64::from(slot.len));
        }
    }
}

impl RollbackEventDiscard for ArenaPresentationIntentJournal {
    fn discard_after(&mut self, retained_through: SimTick) {
        Self::discard_after(self, retained_through);
    }
}

#[derive(Resource)]
pub(crate) struct PowderKegCannonState {
    arena_index: usize,
    fire_timer: TickTimer,
    next_cannon: usize,
}

impl PowderKegCannonState {
    pub(crate) fn new(arena_index: usize) -> Self {
        Self {
            arena_index,
            fire_timer: TickTimer::from_millis_ceil(800),
            next_cannon: 0,
        }
    }

    fn sync_to_arena(&mut self, arena_index: usize) {
        if self.arena_index != arena_index {
            *self = Self::new(arena_index);
        }
    }
}

const ARENA_ROLLBACK_PAYLOAD_VERSION: u8 = 1;
const MAX_ROLLBACK_ARENA_HAZARDS: usize = 3;
const ARENA_DEVICE_CRANK_SAWS_STOPPED: u64 = 1 << 0;
const PIPE_SNAPSHOT_TRANSIT: u8 = 1 << 0;
const PIPE_SNAPSHOT_READY_CANDIDATE: u8 = 1 << 1;
const NO_PIPE_ENDPOINT: u16 = u16::MAX;
const ARENA_PAYLOAD_HEADER_BYTES: usize = 4;
const ARENA_PAYLOAD_HAZARD_BYTES: usize =
    MAX_ROLLBACK_ARENA_HAZARDS * FIGHTER_COUNT * size_of::<u32>();
const ARENA_PAYLOAD_LEVER_OFFSET: usize = ARENA_PAYLOAD_HEADER_BYTES + ARENA_PAYLOAD_HAZARD_BYTES;
const ARENA_PAYLOAD_USED_BYTES: usize = ARENA_PAYLOAD_LEVER_OFFSET + size_of::<u32>();

const _: () = assert!(ARENA_PAYLOAD_USED_BYTES <= ARENA_PAYLOAD_BYTES);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArenaRuntimeSnapshotError {
    MissingResource(&'static str),
    ArenaIndexMismatch {
        resource: &'static str,
        expected: usize,
        actual: usize,
    },
    TooManyHazards {
        count: usize,
        maximum: usize,
    },
    HazardCountMismatch {
        expected: usize,
        actual: usize,
    },
    UnsupportedPayloadVersion(u8),
    NonCanonicalPayloadPadding,
    InvalidLogicalDeviceFlags(u64),
    InvalidPipeFlags(u8),
    InvalidPipeEndpoint(u16),
    InvalidPipePair,
    InvalidPipePadding,
    InconsistentArenaClock,
    InconsistentHazardAggregate {
        fighter: usize,
        expected: u32,
        actual: u32,
    },
}

impl core::fmt::Display for ArenaRuntimeSnapshotError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "arena rollback snapshot failed: {self:?}")
    }
}

impl std::error::Error for ArenaRuntimeSnapshotError {}

pub(crate) struct ArenaRuntimeRestorePlan {
    hazard: ArenaHazardState,
    pipes: ArenaPipeState,
    cannon: PowderKegCannonState,
}

fn arena_resource<'a, T: Resource>(
    world: &'a World,
    name: &'static str,
) -> Result<&'a T, ArenaRuntimeSnapshotError> {
    world
        .get_resource::<T>()
        .ok_or(ArenaRuntimeSnapshotError::MissingResource(name))
}

fn verify_arena_index(
    resource: &'static str,
    expected: usize,
    actual: usize,
) -> Result<(), ArenaRuntimeSnapshotError> {
    if actual == expected {
        Ok(())
    } else {
        Err(ArenaRuntimeSnapshotError::ArenaIndexMismatch {
            resource,
            expected,
            actual,
        })
    }
}

fn write_payload_u32(payload: &mut [u8; ARENA_PAYLOAD_BYTES], offset: usize, value: u32) {
    payload[offset..offset + size_of::<u32>()].copy_from_slice(&value.to_le_bytes());
}

fn read_payload_u32(payload: &[u8; ARENA_PAYLOAD_BYTES], offset: usize) -> u32 {
    u32::from_le_bytes(
        payload[offset..offset + size_of::<u32>()]
            .try_into()
            .expect("fixed arena payload offset is in bounds"),
    )
}

fn capture_pipe_snapshot(state: FighterPipeState) -> FighterPipeSnapshot {
    match state {
        FighterPipeState::Ready {
            candidate,
            dwell_ticks,
            cooldown,
        } => FighterPipeSnapshot {
            flags: if candidate.is_some() {
                PIPE_SNAPSHOT_READY_CANDIDATE
            } else {
                0
            },
            entry_endpoint: candidate.map_or(NO_PIPE_ENDPOINT, |value| value as u16),
            exit_endpoint: NO_PIPE_ENDPOINT,
            dwell_ticks,
            cooldown_ticks: cooldown.remaining(),
            transit_ticks: 0,
            entry_position: QuantizedVec3::default(),
        },
        FighterPipeState::Transit {
            source,
            destination,
            elapsed,
            entry_y,
            base_scale,
        } => FighterPipeSnapshot {
            flags: PIPE_SNAPSHOT_TRANSIT,
            entry_endpoint: source as u16,
            exit_endpoint: destination as u16,
            dwell_ticks: 0,
            cooldown_ticks: 0,
            transit_ticks: elapsed.get(),
            // Pipe transit scale is canonical and uniform. These three fields
            // store entry Y, uniform base scale, and reserved zero respectively.
            entry_position: QuantizedVec3 {
                x: quantize_f32(entry_y, DEFAULT_F32_QUANTIZATION),
                y: quantize_f32(base_scale.x, DEFAULT_F32_QUANTIZATION),
                z: 0,
            },
        },
    }
}

fn restore_pipe_snapshot(
    snapshot: FighterPipeSnapshot,
    has_pipe_pair: bool,
) -> Result<FighterPipeState, ArenaRuntimeSnapshotError> {
    if snapshot.flags & !(PIPE_SNAPSHOT_TRANSIT | PIPE_SNAPSHOT_READY_CANDIDATE) != 0 {
        return Err(ArenaRuntimeSnapshotError::InvalidPipeFlags(snapshot.flags));
    }
    if snapshot.flags & PIPE_SNAPSHOT_TRANSIT != 0 {
        if !has_pipe_pair {
            return Err(ArenaRuntimeSnapshotError::InvalidPipePair);
        }
        if snapshot.flags != PIPE_SNAPSHOT_TRANSIT
            || snapshot.entry_endpoint > 1
            || snapshot.exit_endpoint > 1
            || snapshot.entry_endpoint == snapshot.exit_endpoint
        {
            return Err(ArenaRuntimeSnapshotError::InvalidPipeEndpoint(
                snapshot.entry_endpoint.max(snapshot.exit_endpoint),
            ));
        }
        if snapshot.dwell_ticks != 0
            || snapshot.cooldown_ticks != 0
            || snapshot.entry_position.z != 0
        {
            return Err(ArenaRuntimeSnapshotError::InvalidPipePadding);
        }
        let base_scale = dequantize_f32(snapshot.entry_position.y, DEFAULT_F32_QUANTIZATION);
        if base_scale <= 0.0 {
            return Err(ArenaRuntimeSnapshotError::InvalidPipePadding);
        }
        return Ok(FighterPipeState::Transit {
            source: usize::from(snapshot.entry_endpoint),
            destination: usize::from(snapshot.exit_endpoint),
            elapsed: ElapsedTicks::from_ticks(snapshot.transit_ticks),
            entry_y: dequantize_f32(snapshot.entry_position.x, DEFAULT_F32_QUANTIZATION),
            base_scale: Vec3::splat(base_scale),
        });
    }

    if snapshot.exit_endpoint != NO_PIPE_ENDPOINT
        || snapshot.transit_ticks != 0
        || snapshot.entry_position != QuantizedVec3::default()
    {
        return Err(ArenaRuntimeSnapshotError::InvalidPipePadding);
    }
    let candidate = if snapshot.flags & PIPE_SNAPSHOT_READY_CANDIDATE != 0 {
        if !has_pipe_pair || snapshot.entry_endpoint > 1 {
            return Err(ArenaRuntimeSnapshotError::InvalidPipeEndpoint(
                snapshot.entry_endpoint,
            ));
        }
        Some(usize::from(snapshot.entry_endpoint))
    } else {
        if snapshot.entry_endpoint != NO_PIPE_ENDPOINT {
            return Err(ArenaRuntimeSnapshotError::InvalidPipePadding);
        }
        None
    };
    Ok(FighterPipeState::Ready {
        candidate,
        dwell_ticks: snapshot.dwell_ticks,
        cooldown: TickTimer::from_ticks(snapshot.cooldown_ticks),
    })
}

/// Captures the private arena runtime resources without exposing or duplicating
/// them in the live simulation world.
pub(crate) fn capture_arena_runtime_snapshot(
    world: &World,
) -> Result<ArenaRuntimeSnapshot, ArenaRuntimeSnapshotError> {
    let active = *arena_resource::<ActiveArena>(world, "ActiveArena")?;
    let arena_index = active.index();
    let hazard = arena_resource::<ArenaHazardState>(world, "ArenaHazardState")?;
    let pipes = arena_resource::<ArenaPipeState>(world, "ArenaPipeState")?;
    let cannon = arena_resource::<PowderKegCannonState>(world, "PowderKegCannonState")?;
    verify_arena_index("ArenaHazardState", arena_index, hazard.arena_index)?;
    verify_arena_index("ArenaPipeState", arena_index, pipes.arena_index)?;
    verify_arena_index("PowderKegCannonState", arena_index, cannon.arena_index)?;

    let hazard_count = hazard.hit_cooldowns.len();
    if hazard_count > MAX_ROLLBACK_ARENA_HAZARDS {
        return Err(ArenaRuntimeSnapshotError::TooManyHazards {
            count: hazard_count,
            maximum: MAX_ROLLBACK_ARENA_HAZARDS,
        });
    }
    let mut payload = [0; ARENA_PAYLOAD_BYTES];
    payload[0] = ARENA_ROLLBACK_PAYLOAD_VERSION;
    payload[1] = arena_index as u8;
    payload[2] = hazard_count as u8;
    let mut aggregate = [0; FIGHTER_COUNT];
    for (hazard_index, cooldowns) in hazard.hit_cooldowns.iter().enumerate() {
        for (fighter, cooldown) in cooldowns.iter().enumerate() {
            let ticks = cooldown.remaining();
            aggregate[fighter] = aggregate[fighter].max(ticks);
            let offset = ARENA_PAYLOAD_HEADER_BYTES
                + (hazard_index * FIGHTER_COUNT + fighter) * size_of::<u32>();
            write_payload_u32(&mut payload, offset, ticks);
        }
    }
    write_payload_u32(
        &mut payload,
        ARENA_PAYLOAD_LEVER_OFFSET,
        hazard.crank_lever_toggle_cooldown.remaining(),
    );
    let elapsed = hazard.elapsed.get();
    Ok(ArenaRuntimeSnapshot {
        arena_ticks: u64::from(elapsed),
        hazard_clock_ticks: elapsed,
        logical_device_flags: if hazard.crank_saws_stopped {
            ARENA_DEVICE_CRANK_SAWS_STOPPED
        } else {
            0
        },
        per_fighter_hazard_cooldowns: aggregate,
        cannon_fire_cooldown_ticks: cannon.fire_timer.remaining(),
        cannon_index: cannon.next_cannon as u8,
        pipes: pipes.fighters.map(capture_pipe_snapshot),
        payload,
    })
}

pub(crate) fn prepare_arena_runtime_restore(
    world: &World,
    snapshot: &ArenaRuntimeSnapshot,
) -> Result<ArenaRuntimeRestorePlan, ArenaRuntimeSnapshotError> {
    let active = *arena_resource::<ActiveArena>(world, "ActiveArena")?;
    let arena_index = active.index();
    if snapshot.payload[0] != ARENA_ROLLBACK_PAYLOAD_VERSION {
        return Err(ArenaRuntimeSnapshotError::UnsupportedPayloadVersion(
            snapshot.payload[0],
        ));
    }
    verify_arena_index(
        "snapshot payload",
        arena_index,
        usize::from(snapshot.payload[1]),
    )?;
    let expected_hazards = active.definition().hazards.len();
    let hazard_count = usize::from(snapshot.payload[2]);
    if hazard_count != expected_hazards {
        return Err(ArenaRuntimeSnapshotError::HazardCountMismatch {
            expected: expected_hazards,
            actual: hazard_count,
        });
    }
    if hazard_count > MAX_ROLLBACK_ARENA_HAZARDS {
        return Err(ArenaRuntimeSnapshotError::TooManyHazards {
            count: hazard_count,
            maximum: MAX_ROLLBACK_ARENA_HAZARDS,
        });
    }
    if snapshot.payload[3] != 0
        || snapshot.payload[ARENA_PAYLOAD_USED_BYTES..]
            .iter()
            .any(|byte| *byte != 0)
    {
        return Err(ArenaRuntimeSnapshotError::NonCanonicalPayloadPadding);
    }
    if snapshot.logical_device_flags & !ARENA_DEVICE_CRANK_SAWS_STOPPED != 0 {
        return Err(ArenaRuntimeSnapshotError::InvalidLogicalDeviceFlags(
            snapshot.logical_device_flags,
        ));
    }
    if snapshot.arena_ticks != u64::from(snapshot.hazard_clock_ticks) {
        return Err(ArenaRuntimeSnapshotError::InconsistentArenaClock);
    }

    let mut hit_cooldowns = vec![[TickTimer::ZERO; FIGHTER_COUNT]; hazard_count];
    let mut aggregate = [0; FIGHTER_COUNT];
    for (hazard_index, cooldowns) in hit_cooldowns.iter_mut().enumerate() {
        for (fighter, cooldown) in cooldowns.iter_mut().enumerate() {
            let offset = ARENA_PAYLOAD_HEADER_BYTES
                + (hazard_index * FIGHTER_COUNT + fighter) * size_of::<u32>();
            let ticks = read_payload_u32(&snapshot.payload, offset);
            *cooldown = TickTimer::from_ticks(ticks);
            aggregate[fighter] = aggregate[fighter].max(ticks);
        }
    }
    for (fighter, (expected, actual)) in aggregate
        .into_iter()
        .zip(snapshot.per_fighter_hazard_cooldowns)
        .enumerate()
    {
        if expected != actual {
            return Err(ArenaRuntimeSnapshotError::InconsistentHazardAggregate {
                fighter,
                expected,
                actual,
            });
        }
    }
    let has_pipe_pair = active.definition().pipe_pair.is_some();
    let mut restored_pipes = [FighterPipeState::default(); FIGHTER_COUNT];
    for (destination, source) in restored_pipes.iter_mut().zip(snapshot.pipes) {
        *destination = restore_pipe_snapshot(source, has_pipe_pair)?;
    }
    if snapshot.cannon_index > 1 {
        return Err(ArenaRuntimeSnapshotError::InvalidPipePadding);
    }

    Ok(ArenaRuntimeRestorePlan {
        hazard: ArenaHazardState {
            arena_index,
            elapsed: ElapsedTicks::from_ticks(snapshot.hazard_clock_ticks),
            hit_cooldowns,
            crank_saws_stopped: snapshot.logical_device_flags & ARENA_DEVICE_CRANK_SAWS_STOPPED
                != 0,
            crank_lever_toggle_cooldown: TickTimer::from_ticks(read_payload_u32(
                &snapshot.payload,
                ARENA_PAYLOAD_LEVER_OFFSET,
            )),
        },
        pipes: ArenaPipeState {
            arena_index,
            fighters: restored_pipes,
        },
        cannon: PowderKegCannonState {
            arena_index,
            fire_timer: TickTimer::from_ticks(snapshot.cannon_fire_cooldown_ticks),
            next_cannon: usize::from(snapshot.cannon_index),
        },
    })
}

pub(crate) fn commit_arena_runtime_restore(world: &mut World, plan: ArenaRuntimeRestorePlan) {
    world.insert_resource(plan.hazard);
    world.insert_resource(plan.pipes);
    world.insert_resource(plan.cannon);
}

/// Installs the authoritative arena selection and its rollback-relevant runtime
/// resources in a bare simulation [`World`]. This deliberately does not create
/// [`ArenaScene`], [`ArenaGeometry`], render assets, lights, or any other
/// presentation state, so dedicated and in-process authority worlds can share
/// the exact gameplay bootstrap without an asset server or renderer.
///
/// Repeating the call for the currently selected arena preserves live runtime
/// state. Selecting a different arena resets all arena-local clocks, cooldowns,
/// pipe transits, and cannon sequencing to that arena's deterministic defaults.
pub fn bootstrap_canonical_arena_runtime(world: &mut World, arena_index: usize) -> ActiveArena {
    let mut selected = world
        .get_resource::<ActiveArena>()
        .copied()
        .unwrap_or_default();
    selected.select(arena_index);
    world.insert_resource(selected);

    let selected_index = selected.index();
    let hazard_count = selected.definition().hazards.len();
    if let Some(mut state) = world.get_resource_mut::<ArenaHazardState>() {
        state.sync_to_arena(selected_index, hazard_count);
    } else {
        world.insert_resource(ArenaHazardState::new(selected_index, hazard_count));
    }
    if let Some(mut state) = world.get_resource_mut::<ArenaPipeState>() {
        state.sync_to_arena(selected_index);
    } else {
        world.insert_resource(ArenaPipeState::new(selected_index));
    }
    if let Some(mut state) = world.get_resource_mut::<PowderKegCannonState>() {
        state.sync_to_arena(selected_index);
    } else {
        world.insert_resource(PowderKegCannonState::new(selected_index));
    }

    selected
}

pub fn setup_arena(
    mut commands: Commands,
    active_arena: Res<ActiveArena>,
    asset_server: Res<AssetServer>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let arena_index = active_arena.index();
    let arena = active_arena.definition();
    let generation = ArenaScene::INITIAL_GENERATION;
    commands.insert_resource(ArenaScene::new(arena_index));
    commands.insert_resource(ArenaHazardState::new(arena_index, arena.hazards.len()));
    commands.insert_resource(ArenaPipeState::new(arena_index));
    commands.insert_resource(PowderKegCannonState::new(arena_index));
    commands.insert_resource(ArenaOrdnanceAssets {
        bomb_mesh: meshes.add(Sphere::new(0.34).mesh().uv(14, 8)),
        bomb_material: materials.add(StandardMaterial {
            base_color: Color::srgb(0.08, 0.07, 0.065),
            emissive: LinearRgba::rgb(0.5, 0.11, 0.015),
            metallic: 0.48,
            perceptual_roughness: 0.34,
            ..default()
        }),
    });
    spawn_arena_geometry(
        &mut commands,
        &asset_server,
        &mut meshes,
        &mut materials,
        arena_index,
        generation,
        arena,
    );
    spawn_arena_lights(&mut commands);
}

pub fn sync_arena_visuals(
    mut commands: Commands,
    active_arena: Res<ActiveArena>,
    mut scene: ResMut<ArenaScene>,
    geometry_roots: Query<Entity, (With<ArenaGeometry>, Without<ChildOf>)>,
    asset_server: Res<AssetServer>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let selected = active_arena.index();
    if scene.index == selected {
        return;
    }

    // Despawning a hierarchy root recursively removes its descendants. Querying
    // every marked child as well caused Bevy B0004 warnings when later commands
    // attempted to despawn children whose parent had already gone away.
    for entity in &geometry_roots {
        commands.entity(entity).despawn();
    }
    let generation = scene.rebuild(selected);
    spawn_arena_geometry(
        &mut commands,
        &asset_server,
        &mut meshes,
        &mut materials,
        selected,
        generation,
        active_arena.definition(),
    );
}

fn spawn_arena_geometry(
    commands: &mut Commands,
    asset_server: &AssetServer,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    arena_index: usize,
    generation: u64,
    arena: &ArenaDefinition,
) {
    commands.spawn((
        ArenaGeometry,
        ArenaSceneReadyMarker {
            arena_index,
            generation,
        },
        Name::new(format!(
            "Arena scene ready marker {arena_index}:{generation}"
        )),
    ));
    spawn_arena_background(
        commands,
        asset_server,
        meshes,
        materials,
        arena.background,
        arena.camera_offset,
    );

    let palette = arena_theme_palette(arena.visual_theme);
    let hazard_material = materials.add(StandardMaterial {
        base_color: palette.hazard.with_alpha(0.34),
        emissive: LinearRgba::from(palette.hazard) * 0.16,
        alpha_mode: AlphaMode::Blend,
        depth_bias: 16.0,
        ..default()
    });

    if arena_index == CHAMPIONS_COURT_ARENA_INDEX {
        match spawn_champions_court_map(commands, asset_server) {
            Ok(()) => {
                spawn_arena_hazard_markers(
                    commands,
                    meshes,
                    hazard_material,
                    arena_index,
                    arena.hazards,
                );
                return;
            }
            Err(error) => {
                warn!("Could not load {CHAMPIONS_COURT_RON_PATH}: {error}");
            }
        }
    }

    let primary = materials.add(StandardMaterial {
        base_color: palette.primary,
        perceptual_roughness: 0.9,
        ..default()
    });
    let secondary = materials.add(StandardMaterial {
        base_color: palette.secondary,
        perceptual_roughness: 0.84,
        ..default()
    });
    let trim = materials.add(StandardMaterial {
        base_color: palette.trim,
        metallic: matches!(
            arena.visual_theme,
            ArenaVisualTheme::Industrial | ArenaVisualTheme::Reactor
        )
        .then_some(0.22)
        .unwrap_or(0.02),
        perceptual_roughness: 0.64,
        ..default()
    });

    spawn_arena_ground_shapes(
        commands,
        meshes,
        materials,
        primary.clone(),
        secondary.clone(),
        arena,
    );
    if arena_index == VENT_SPIRAL_ARENA_INDEX {
        spawn_vent_spiral_platform_blocks(commands, meshes, materials, arena.platforms);
    } else {
        spawn_platform_blocks(commands, meshes, materials, secondary, arena.platforms);
    }
    spawn_arena_theme_accents(commands, meshes, trim, arena.visual_theme);
    spawn_arena_hazard_markers(
        commands,
        meshes,
        hazard_material,
        arena_index,
        arena.hazards,
    );
    spawn_campfire_props(commands, meshes, materials, arena.hazards);
    spawn_mini_arena_props(commands, asset_server, arena_index);
    spawn_vent_spiral_machinery(
        commands,
        asset_server,
        meshes,
        materials,
        arena_index,
        arena.hazards,
    );
    spawn_crank_yard_machinery(
        commands,
        asset_server,
        meshes,
        materials,
        arena_index,
        arena.hazards,
    );
}

fn arena_theme_palette(theme: ArenaVisualTheme) -> ArenaThemePalette {
    match theme {
        ArenaVisualTheme::Crown => ArenaThemePalette {
            primary: Color::srgb(0.66, 0.57, 0.42),
            secondary: Color::srgb(0.82, 0.76, 0.59),
            trim: Color::srgb(0.76, 0.08, 0.055),
            hazard: Color::srgb(0.1, 0.8, 0.65),
        },
        ArenaVisualTheme::Causeway => ArenaThemePalette {
            primary: Color::srgb(0.36, 0.55, 0.34),
            secondary: Color::srgb(0.48, 0.34, 0.2),
            trim: Color::srgb(0.86, 0.67, 0.24),
            hazard: Color::srgb(0.98, 0.28, 0.08),
        },
        ArenaVisualTheme::Terrace => ArenaThemePalette {
            primary: Color::srgb(0.58, 0.62, 0.36),
            secondary: Color::srgb(0.76, 0.61, 0.35),
            trim: Color::srgb(0.86, 0.42, 0.16),
            hazard: Color::srgb(0.8, 0.24, 0.16),
        },
        ArenaVisualTheme::Industrial => ArenaThemePalette {
            primary: Color::srgb(0.31, 0.35, 0.38),
            secondary: Color::srgb(0.48, 0.52, 0.52),
            trim: Color::srgb(0.96, 0.66, 0.12),
            hazard: Color::srgb(0.98, 0.28, 0.12),
        },
        ArenaVisualTheme::Reactor => ArenaThemePalette {
            primary: Color::srgb(0.16, 0.22, 0.21),
            secondary: Color::srgb(0.42, 0.47, 0.46),
            trim: Color::srgb(0.18, 0.9, 0.78),
            hazard: Color::srgb(1.0, 0.32, 0.12),
        },
        ArenaVisualTheme::Toybox => ArenaThemePalette {
            primary: Color::srgb(0.2, 0.55, 0.86),
            secondary: Color::srgb(0.96, 0.38, 0.2),
            trim: Color::srgb(1.0, 0.82, 0.18),
            hazard: Color::srgb(1.0, 0.32, 0.52),
        },
        ArenaVisualTheme::Market => ArenaThemePalette {
            primary: Color::srgb(0.72, 0.52, 0.3),
            secondary: Color::srgb(0.4, 0.61, 0.38),
            trim: Color::srgb(0.9, 0.18, 0.13),
            hazard: Color::srgb(0.96, 0.64, 0.12),
        },
        ArenaVisualTheme::Garden => ArenaThemePalette {
            primary: Color::srgb(0.34, 0.63, 0.3),
            secondary: Color::srgb(0.55, 0.72, 0.4),
            trim: Color::srgb(0.94, 0.48, 0.62),
            hazard: Color::srgb(0.63, 0.24, 0.76),
        },
        ArenaVisualTheme::Snow => ArenaThemePalette {
            primary: Color::srgb(0.84, 0.92, 0.94),
            secondary: Color::srgb(0.49, 0.72, 0.79),
            trim: Color::srgb(0.92, 0.24, 0.2),
            hazard: Color::srgb(0.2, 0.72, 0.96),
        },
        ArenaVisualTheme::Powder => ArenaThemePalette {
            primary: Color::srgb(0.43, 0.32, 0.23),
            secondary: Color::srgb(0.24, 0.25, 0.25),
            trim: Color::srgb(0.9, 0.55, 0.12),
            hazard: Color::srgb(0.96, 0.2, 0.08),
        },
    }
}

fn spawn_arena_ground_shapes(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    primary: Handle<StandardMaterial>,
    secondary: Handle<StandardMaterial>,
    arena: &ArenaDefinition,
) {
    for (index, shape) in arena.ground_shapes.iter().enumerate() {
        let base_material = if index % 2 == 0 { &primary } else { &secondary };
        let material =
            material_with_depth_bias(materials, base_material, arena_ground_depth_bias(index));
        let (mesh, transform) = match *shape {
            ArenaGroundShape::Circle {
                center,
                radius,
                top_y,
            } => (
                meshes.add(Cylinder::new(radius, ARENA_HEIGHT)),
                Transform::from_xyz(center.x, top_y - ARENA_HEIGHT * 0.5, center.y),
            ),
            ArenaGroundShape::Rectangle {
                center,
                half_extents,
                yaw,
                top_y,
            } => (
                meshes.add(Cuboid::new(
                    half_extents.x * 2.0,
                    ARENA_HEIGHT,
                    half_extents.y * 2.0,
                )),
                Transform::from_xyz(center.x, top_y - ARENA_HEIGHT * 0.5, center.y)
                    .with_rotation(Quat::from_rotation_y(yaw)),
            ),
        };
        commands.spawn((
            Mesh3d(mesh),
            MeshMaterial3d(material),
            transform,
            Name::new(format!("{} ground {index}", arena.name)),
            ArenaGeometry,
        ));
    }
}

fn spawn_platform_blocks(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    material: Handle<StandardMaterial>,
    platforms: &[PlatformDefinition],
) {
    for (index, platform) in platforms.iter().enumerate() {
        let height = ARENA_HEIGHT + (platform.top_y - ARENA_TOP_Y).max(0.0);
        let platform_material =
            material_with_depth_bias(materials, &material, arena_platform_depth_bias(index));
        commands.spawn((
            Mesh3d(meshes.add(platform.block_mesh(height))),
            MeshMaterial3d(platform_material),
            platform.block_transform(height),
            Name::new(format!("Arena platform {index}")),
            ArenaGeometry,
        ));
    }
}

fn spawn_vent_spiral_platform_blocks(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    platforms: &[PlatformDefinition],
) {
    let tier_materials = [
        materials.add(StandardMaterial {
            base_color: Color::srgb(0.34, 0.39, 0.38),
            metallic: 0.18,
            perceptual_roughness: 0.72,
            ..default()
        }),
        materials.add(StandardMaterial {
            base_color: Color::srgb(0.25, 0.38, 0.46),
            metallic: 0.16,
            perceptual_roughness: 0.7,
            ..default()
        }),
        materials.add(StandardMaterial {
            base_color: Color::srgb(0.25, 0.43, 0.34),
            metallic: 0.14,
            perceptual_roughness: 0.74,
            ..default()
        }),
        materials.add(StandardMaterial {
            base_color: Color::srgb(0.5, 0.46, 0.35),
            metallic: 0.12,
            perceptual_roughness: 0.76,
            ..default()
        }),
    ];

    for (index, platform) in platforms.iter().enumerate() {
        let tier = (((platform.top_y - ARENA_TOP_Y) / 0.65).round() as usize).min(3);
        let height = ARENA_HEIGHT + (platform.top_y - ARENA_TOP_Y).max(0.0);
        let material = material_with_depth_bias(
            materials,
            &tier_materials[tier],
            arena_platform_depth_bias(index),
        );
        commands.spawn((
            Mesh3d(meshes.add(platform.block_mesh(height))),
            MeshMaterial3d(material),
            platform.block_transform(height),
            Name::new(format!("Vent spiral tier {tier} block {index}")),
            ArenaGeometry,
        ));
    }

    spawn_vent_spiral_transition_marks(commands, meshes, materials);
}

fn spawn_vent_spiral_transition_marks(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
) {
    let warning_material = materials.add(StandardMaterial {
        base_color: Color::srgb(1.0, 0.62, 0.08),
        emissive: LinearRgba::rgb(0.45, 0.19, 0.015),
        metallic: 0.12,
        perceptual_roughness: 0.58,
        ..default()
    });
    let marker_mesh = meshes.add(Cuboid::new(0.42, 0.045, 0.13));
    let transitions = [
        (Vec3::new(4.15, ARENA_TOP_Y + 0.678, 3.16), 0.0),
        (Vec3::new(-3.9, ARENA_TOP_Y + 1.328, 3.56), PI * 0.5),
        (Vec3::new(-3.75, ARENA_TOP_Y + 1.978, -2.48), 0.0),
    ];

    for (transition_index, (center, yaw)) in transitions.into_iter().enumerate() {
        for stripe in -1..=1 {
            let offset = Quat::from_rotation_y(yaw) * Vec3::new(stripe as f32 * 0.52, 0.0, 0.0);
            commands.spawn((
                Mesh3d(marker_mesh.clone()),
                MeshMaterial3d(warning_material.clone()),
                Transform::from_translation(center + offset)
                    .with_rotation(Quat::from_rotation_y(yaw)),
                Name::new(format!(
                    "Vent spiral jump marker {transition_index}-{stripe}"
                )),
                ArenaGeometry,
            ));
        }
    }
}

fn material_with_depth_bias(
    materials: &mut Assets<StandardMaterial>,
    source: &Handle<StandardMaterial>,
    depth_bias: f32,
) -> Handle<StandardMaterial> {
    let mut material = materials
        .get(source)
        .cloned()
        .expect("arena base material should exist before geometry is spawned");
    material.depth_bias = depth_bias;
    materials.add(material)
}

fn arena_ground_depth_bias(index: usize) -> f32 {
    ARENA_GROUND_DEPTH_BIAS_BASE + index as f32 * ARENA_GROUND_DEPTH_BIAS_STEP
}

fn arena_platform_depth_bias(index: usize) -> f32 {
    ARENA_PLATFORM_DEPTH_BIAS_BASE + index as f32 * ARENA_PLATFORM_DEPTH_BIAS_STEP
}

fn spawn_arena_theme_accents(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    trim: Handle<StandardMaterial>,
    theme: ArenaVisualTheme,
) {
    let (positions, size) = match theme {
        ArenaVisualTheme::Causeway => (&[(-4.7, 0.0), (4.7, 0.0)][..], Vec2::new(0.16, 11.5)),
        ArenaVisualTheme::Terrace => (&[(-2.2, 1.45), (2.2, -1.45)][..], Vec2::new(3.6, 0.12)),
        ArenaVisualTheme::Industrial => (&[(0.0, -1.8), (0.0, 1.8)][..], Vec2::new(13.0, 0.16)),
        ArenaVisualTheme::Reactor => return,
        ArenaVisualTheme::Toybox => (&[(-2.8, 0.0), (2.8, 0.0)][..], Vec2::new(0.2, 15.2)),
        ArenaVisualTheme::Market => (&[(0.0, 0.0)][..], Vec2::new(8.8, 0.18)),
        ArenaVisualTheme::Garden => (&[(-3.4, 0.0), (3.4, 0.0)][..], Vec2::new(0.12, 2.8)),
        ArenaVisualTheme::Snow => (&[(-2.9, -2.3), (2.9, 2.3)][..], Vec2::new(3.4, 0.14)),
        ArenaVisualTheme::Powder => (&[(-3.8, 0.0), (3.8, 0.0)][..], Vec2::new(0.18, 12.0)),
        ArenaVisualTheme::Crown => return,
    };

    for (x, z) in positions {
        commands.spawn((
            Mesh3d(meshes.add(Cuboid::new(size.x, 0.035, size.y))),
            MeshMaterial3d(trim.clone()),
            Transform::from_xyz(*x, ARENA_TOP_Y + 0.025, *z),
            ArenaGeometry,
        ));
    }
}

fn arena_background_wallpaper_size(background: ArenaBackgroundDefinition) -> Vec2 {
    Vec2::new(
        background.world_height * background.image_size.x / background.image_size.y,
        background.world_height,
    )
}

fn arena_background_wallpaper_transform(
    background: ArenaBackgroundDefinition,
    camera_transform: &Transform,
) -> Transform {
    Transform::from_translation(
        camera_transform.translation + camera_transform.forward() * background.distance,
    )
    .with_rotation(camera_transform.rotation)
}

fn spawn_arena_background(
    commands: &mut Commands,
    asset_server: &AssetServer,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    background: ArenaBackgroundDefinition,
    camera_offset: Vec3,
) {
    let size = arena_background_wallpaper_size(background);
    let camera_transform =
        Transform::from_translation(camera_offset).looking_at(Vec3::Y * 0.6, Vec3::Y);
    let material = materials.add(StandardMaterial {
        base_color_texture: Some(asset_server.load(background.asset_path)),
        unlit: true,
        cull_mode: None,
        perceptual_roughness: 1.0,
        ..default()
    });

    commands.spawn((
        Mesh3d(meshes.add(Rectangle::new(size.x, size.y))),
        MeshMaterial3d(material),
        arena_background_wallpaper_transform(background, &camera_transform),
        ArenaBackgroundWallpaper(background),
        Name::new("Arena scenic wallpaper"),
        ArenaGeometry,
    ));
}

pub fn sync_arena_background_to_camera(
    camera: Query<&Transform, (With<ArenaCamera>, Without<ArenaBackgroundWallpaper>)>,
    mut backgrounds: Query<
        (&ArenaBackgroundWallpaper, &mut Transform),
        (Without<ArenaCamera>, With<ArenaGeometry>),
    >,
) {
    let Ok(camera_transform) = camera.single() else {
        return;
    };

    for (background, mut transform) in &mut backgrounds {
        *transform = arena_background_wallpaper_transform(background.0, camera_transform);
    }
}

fn spawn_champions_court_map(
    commands: &mut Commands,
    asset_server: &AssetServer,
) -> Result<(), String> {
    let map = load_champions_court_map()?;
    let mut scenes = HashMap::new();

    spawn_champions_floor_shapes(commands, asset_server, &map, &mut scenes);

    for object in &map.instances {
        spawn_champions_object(
            commands,
            asset_server,
            &map,
            &mut scenes,
            &object.asset,
            champions_object_transform(object),
            champions_object_name("Champions Court object", &object.id, &object.asset),
        );
    }

    for prefab_instance in &map.prefab_instances {
        let Some(prefab) = map.prefabs.get(&prefab_instance.prefab) else {
            warn!(
                "Champion's Court prefab instance '{}' references missing prefab '{}'",
                prefab_instance.id, prefab_instance.prefab
            );
            continue;
        };

        for object in prefab {
            spawn_champions_object(
                commands,
                asset_server,
                &map,
                &mut scenes,
                &object.asset,
                champions_prefab_object_transform(prefab_instance, object),
                champions_prefab_object_name(prefab_instance, object),
            );
        }
    }

    if CHAMPIONS_COURT_MAP_LIGHTS_ENABLED {
        spawn_champions_lights(commands, &map.lights);
    }

    Ok(())
}

fn load_champions_court_map() -> Result<ChampionsCourtRon, String> {
    load_champions_court_map_from_path(Path::new(CHAMPIONS_COURT_RON_PATH))
}

#[cfg(all(
    feature = "dev-hot-reload",
    not(feature = "shipping"),
    not(target_arch = "wasm32")
))]
fn load_champions_court_map_from_path(path: &Path) -> Result<ChampionsCourtRon, String> {
    let contents = fs::read_to_string(path).map_err(|error| format!("read failed: {error}"))?;
    ron::from_str(&contents).map_err(|error| format!("RON parse failed: {error}"))
}

#[cfg(not(all(
    feature = "dev-hot-reload",
    not(feature = "shipping"),
    not(target_arch = "wasm32")
)))]
fn load_champions_court_map_from_path(_path: &Path) -> Result<ChampionsCourtRon, String> {
    ron::from_str(EMBEDDED_CHAMPIONS_COURT_RON)
        .map_err(|error| format!("RON parse failed: {error}"))
}

fn spawn_champions_floor_shapes(
    commands: &mut Commands,
    asset_server: &AssetServer,
    map: &ChampionsCourtRon,
    scenes: &mut HashMap<String, Handle<Scene>>,
) {
    for shape in &map.floor_shapes {
        let Some(scene) = champions_scene_handle(asset_server, map, scenes, &shape.asset) else {
            warn!(
                "Champion's Court floor shape '{}' references missing asset '{}'",
                shape.id, shape.asset
            );
            continue;
        };

        let scale = Vec3::splat(champions_floor_asset_scale(&shape.asset, map.map.tile_size));
        for tile in champions_floor_shape_render_positions(
            shape,
            map.map.tile_size,
            CHAMPIONS_COURT_ARENA_INDEX,
        ) {
            let x = tile.x;
            let z = tile.y;
            commands.spawn((
                SceneRoot(scene.clone()),
                Transform::from_xyz(x, champions_stage_y(shape.y), z)
                    .with_rotation(champions_yaw(shape.rotation_y))
                    .with_scale(scale),
                ArenaGeometry,
                Name::new(format!("Champion's Court floor {}", shape.id)),
            ));
        }
    }
}

#[allow(dead_code)]
fn champions_floor_render_asset(
    asset_server: &AssetServer,
    map: &ChampionsCourtRon,
    render_assets: &mut HashMap<String, ChampionsCourtFloorRenderAsset>,
    asset_key: &str,
) -> Option<ChampionsCourtFloorRenderAsset> {
    let path = champions_runtime_asset_path(&map.assets, asset_key)?;
    if let Some(asset) = render_assets.get(&path) {
        return Some(asset.clone());
    }

    let asset = ChampionsCourtFloorRenderAsset {
        mesh: asset_server.load(
            GltfAssetLabel::Primitive {
                mesh: 0,
                primitive: 0,
            }
            .from_asset(path.clone()),
        ),
        material: asset_server.load(
            GltfAssetLabel::Material {
                index: 0,
                is_scale_inverted: false,
            }
            .from_asset(path.clone()),
        ),
    };
    render_assets.insert(path, asset.clone());
    Some(asset)
}

fn spawn_champions_object(
    commands: &mut Commands,
    asset_server: &AssetServer,
    map: &ChampionsCourtRon,
    scenes: &mut HashMap<String, Handle<Scene>>,
    asset_key: &str,
    transform: Transform,
    name: String,
) {
    let Some(scene) = champions_scene_handle(asset_server, map, scenes, asset_key) else {
        warn!("Champion's Court object '{name}' references missing asset '{asset_key}'");
        return;
    };

    commands.spawn((SceneRoot(scene), transform, ArenaGeometry, Name::new(name)));
}

fn champions_scene_handle(
    asset_server: &AssetServer,
    map: &ChampionsCourtRon,
    scenes: &mut HashMap<String, Handle<Scene>>,
    asset_key: &str,
) -> Option<Handle<Scene>> {
    let path = champions_runtime_asset_path(&map.assets, asset_key)?;
    if let Some(scene) = scenes.get(&path) {
        return Some(scene.clone());
    }

    let scene = asset_server.load(GltfAssetLabel::Scene(0).from_asset(path.clone()));
    scenes.insert(path, scene.clone());
    Some(scene)
}

fn champions_runtime_asset_path(
    assets: &HashMap<String, String>,
    asset_key: &str,
) -> Option<String> {
    assets
        .get(asset_key)
        .map(|file| format!("{MINI_ARENA_ASSET_ROOT}/{file}"))
}

fn champions_object_transform(object: &ChampionsCourtObject) -> Transform {
    Transform::from_translation(champions_object_position(object.position))
        .with_rotation(champions_yaw(object.rotation_y))
        .with_scale(champions_scale(object.scale))
}

fn champions_prefab_object_transform(
    prefab_instance: &ChampionsCourtPrefabInstance,
    object: &ChampionsCourtObject,
) -> Transform {
    let parent_rotation = champions_yaw(prefab_instance.rotation_y);
    let child_rotation = champions_yaw(object.rotation_y);
    let parent_scale = champions_scale(prefab_instance.scale);
    let child_scale = champions_scale(object.scale);
    let parent_position = champions_raw_position(prefab_instance.position);
    let child_position = champions_raw_position(object.position);
    let translation = parent_position + parent_rotation * (child_position * parent_scale);

    Transform::from_translation(Vec3::new(
        translation.x,
        champions_stage_y(translation.y) + ARENA_PROP_SURFACE_CLEARANCE,
        translation.z,
    ))
    .with_rotation(parent_rotation * child_rotation)
    .with_scale(parent_scale * child_scale)
}

fn champions_object_name(prefix: &str, id: &str, asset_key: &str) -> String {
    if id.is_empty() {
        format!("{prefix} {asset_key}")
    } else {
        format!("{prefix} {id}")
    }
}

fn champions_prefab_object_name(
    prefab_instance: &ChampionsCourtPrefabInstance,
    object: &ChampionsCourtObject,
) -> String {
    if object.id.is_empty() {
        format!(
            "Champions Court prefab {} {}",
            prefab_instance.id, object.asset
        )
    } else {
        format!(
            "Champions Court prefab {} {}",
            prefab_instance.id, object.id
        )
    }
}

fn champions_floor_shape_tiles(shape: &ChampionsCourtFloorShape) -> Vec<Vec2> {
    match shape.kind.as_str() {
        "filled_octagon" => {
            if shape.radius_tiles <= 0 {
                return Vec::new();
            }
            champions_octagon_tiles(shape.radius_tiles, None)
        }
        "octagon_ring" => {
            if shape.outer_radius_tiles <= 0 {
                return Vec::new();
            }
            champions_octagon_tiles(
                shape.outer_radius_tiles,
                (shape.inner_radius_tiles > 0).then_some(shape.inner_radius_tiles),
            )
        }
        "rectangle" => {
            let (width, depth) = shape.size_tiles;
            if width <= 0 || depth <= 0 {
                return Vec::new();
            }
            champions_rectangle_tiles(width, depth)
        }
        _ => Vec::new(),
    }
}

fn champions_floor_shape_render_positions(
    shape: &ChampionsCourtFloorShape,
    tile_size: f32,
    arena_index: usize,
) -> Vec<Vec2> {
    champions_floor_shape_tiles(shape)
        .into_iter()
        .map(|tile| {
            Vec2::new(
                (shape.center.0 as f32 + tile.x) * tile_size,
                (shape.center.1 as f32 + tile.y) * tile_size,
            )
        })
        .filter(|position| floor_tile_is_firm_supported(arena_index, position.x, position.y))
        .collect()
}

fn champions_octagon_tiles(outer_radius: i32, inner_radius: Option<i32>) -> Vec<Vec2> {
    let outer_radius = outer_radius.max(0);
    let mut tiles = Vec::new();
    for x in -outer_radius..=outer_radius {
        for z in -outer_radius..=outer_radius {
            let distance = champions_octagon_distance(x, z);
            if distance > outer_radius as f32 {
                continue;
            }
            if let Some(inner_radius) = inner_radius {
                if distance <= inner_radius.max(0) as f32 {
                    continue;
                }
            }
            tiles.push(Vec2::new(x as f32, z as f32));
        }
    }
    tiles
}

fn champions_octagon_distance(x: i32, z: i32) -> f32 {
    let abs_x = x.abs() as f32;
    let abs_z = z.abs() as f32;
    abs_x.max(abs_z) + abs_x.min(abs_z) * 0.414
}

fn champions_rectangle_tiles(width: i32, depth: i32) -> Vec<Vec2> {
    let width = width.max(0);
    let depth = depth.max(0);
    let x_offset = (width - 1) as f32 * 0.5;
    let z_offset = (depth - 1) as f32 * 0.5;
    let mut tiles = Vec::new();
    for x in 0..width {
        for z in 0..depth {
            tiles.push(Vec2::new(x as f32 - x_offset, z as f32 - z_offset));
        }
    }
    tiles
}

fn spawn_champions_lights(commands: &mut Commands, lights: &[ChampionsCourtLight]) {
    for light in lights {
        match light.kind.as_str() {
            "directional" => {
                commands.spawn((
                    DirectionalLight {
                        illuminance: if light.illuminance > 0.0 {
                            light.illuminance
                        } else {
                            12_500.0
                        },
                        color: champions_color(light.color),
                        shadows_enabled: light.shadows,
                        ..default()
                    },
                    champions_light_transform(light),
                    ArenaGeometry,
                    Name::new(format!("Champion's Court light {}", light.id)),
                ));
            }
            "point" => {
                commands.spawn((
                    PointLight {
                        intensity: if light.intensity > 0.0 {
                            light.intensity
                        } else {
                            850.0
                        } * CHAMPIONS_COURT_LIGHT_SCALE,
                        range: if light.range > 0.0 { light.range } else { 8.0 },
                        color: champions_color(light.color),
                        shadows_enabled: light.shadows,
                        ..default()
                    },
                    Transform::from_translation(champions_stage_position(light.position)),
                    ArenaGeometry,
                    Name::new(format!("Champion's Court light {}", light.id)),
                ));
            }
            _ => {}
        }
    }
}

fn champions_light_transform(light: &ChampionsCourtLight) -> Transform {
    let (x, y, z) = light.rotation_euler_degrees;
    Transform::from_rotation(Quat::from_euler(
        EulerRot::XYZ,
        x.to_radians(),
        y.to_radians(),
        z.to_radians(),
    ))
}

fn champions_color(color: (f32, f32, f32)) -> Color {
    let (r, g, b) = color;
    Color::srgb(r, g, b)
}

fn champions_floor_asset_scale(asset_key: &str, tile_size: f32) -> f32 {
    let base_scale = match asset_key {
        "floor_detail" => 1.42,
        _ => MINI_ARENA_FLOOR_SCALE,
    };
    base_scale * tile_size / MINI_ARENA_FLOOR_SPACING
}

fn champions_stage_position(position: (f32, f32, f32)) -> Vec3 {
    let position = champions_raw_position(position);
    Vec3::new(position.x, champions_stage_y(position.y), position.z)
}

fn champions_object_position(position: (f32, f32, f32)) -> Vec3 {
    champions_stage_position(position) + Vec3::Y * ARENA_PROP_SURFACE_CLEARANCE
}

fn champions_raw_position(position: (f32, f32, f32)) -> Vec3 {
    Vec3::new(position.0, position.1, position.2)
}

fn champions_stage_y(y: f32) -> f32 {
    ARENA_TOP_Y + y
}

fn champions_yaw(degrees: f32) -> Quat {
    Quat::from_rotation_y(degrees.to_radians())
}

fn champions_scale(scale: (f32, f32, f32)) -> Vec3 {
    Vec3::new(scale.0, scale.1, scale.2)
}

fn unit_tuple3() -> (f32, f32, f32) {
    (1.0, 1.0, 1.0)
}

fn white_tuple3() -> (f32, f32, f32) {
    (1.0, 1.0, 1.0)
}

fn spawn_mini_arena_props(commands: &mut Commands, asset_server: &AssetServer, arena_index: usize) {
    for prop in arena_asset_props(arena_index) {
        let asset_path = arena_prop_asset_path(prop.file);
        commands.spawn((
            SceneRoot(asset_server.load(GltfAssetLabel::Scene(0).from_asset(asset_path))),
            prop.transform(),
            ArenaGeometry,
            Name::new(prop.name),
        ));
    }
}

fn arena_prop_asset_path(file: &str) -> String {
    if file.contains('/') {
        format!("{ARENA_KIT_ASSET_ROOT}/{file}")
    } else {
        format!("{MINI_ARENA_ASSET_ROOT}/{file}")
    }
}

fn floor_tile_is_firm_supported(arena_index: usize, x: f32, z: f32) -> bool {
    let definitions = arena_definitions();
    let Some(arena) = definitions.get(arena_index.min(definitions.len().saturating_sub(1))) else {
        return false;
    };
    arena_position_is_firm_supported(arena, x, z)
}

fn arena_position_is_firm_supported(arena: &ArenaDefinition, x: f32, z: f32) -> bool {
    arena
        .ground_shapes
        .iter()
        .any(|shape| ground_shape_contains_firm_support(shape, x, z))
        || arena
            .platforms
            .iter()
            .any(|platform| platform_contains_firm_support(platform, x, z))
}

fn ground_shape_contains_firm_support(shape: &ArenaGroundShape, x: f32, z: f32) -> bool {
    ground_shape_support(shape, x, z, 0.0).is_some()
}

fn platform_contains_firm_support(platform: &PlatformDefinition, x: f32, z: f32) -> bool {
    let dx = (x - platform.center.x).abs();
    let dz = (z - platform.center.y).abs();
    dx <= platform.half_extents.x && dz <= platform.half_extents.y
}

fn arena_asset_props(arena_index: usize) -> &'static [ArenaAssetProp] {
    match arena_index {
        0 => CROWN_ASSET_PROPS,
        1 => SPLIT_ASSET_PROPS,
        2 => SUNSTONE_ASSET_PROPS,
        3 => CRANK_ASSET_PROPS,
        4 => VENT_SPIRAL_ASSET_PROPS,
        5 => BUMPER_ALLEY_ASSET_PROPS,
        6 => FEAST_MARKET_ASSET_PROPS,
        7 => SNARE_GARDEN_ASSET_PROPS,
        8 => SKY_STEPS_ASSET_PROPS,
        9 => POWDER_KEG_ASSET_PROPS,
        _ => CROWN_ASSET_PROPS,
    }
}

fn arena_asset_props_for_definition(arena: &ArenaDefinition) -> &'static [ArenaAssetProp] {
    let arena_index = arena_definitions()
        .iter()
        .position(|candidate| candidate.name == arena.name)
        .unwrap_or(CHAMPIONS_COURT_ARENA_INDEX);

    // Champions Court is rendered from its RON scene rather than this fallback prop list.
    if arena_index == CHAMPIONS_COURT_ARENA_INDEX {
        &[]
    } else {
        arena_asset_props(arena_index)
    }
}

// Frozen from the pre-C1 production RON/quaternion/Euler collision builder on the
// reference toolchain documented in canonical_math. Presentation still consumes
// the RON; canonical collision consumes only these final world-space records.
const CHAMPIONS_COURT_COLLISION_BARRIERS: [WorldPropBarrier; 91] = [
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0x00000000),
            f32::from_bits(0x00000000),
            f32::from_bits(0x3eaf1aa0),
            f32::from_bits(0x3e8adaba),
            f32::from_bits(0x00000000),
            f32::from_bits(0x3fb50b10),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0x00000000),
            f32::from_bits(0x00000000),
            f32::from_bits(0x3f733333),
            f32::from_bits(0x3f733333),
            f32::from_bits(0x3f490fdc),
            f32::from_bits(0x3f2fdf3c),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::circle(
            f32::from_bits(0xc06ccccd),
            f32::from_bits(0x406ccccd),
            f32::from_bits(0x3e9a9fbe),
            f32::from_bits(0x3fbb22d1),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::circle(
            f32::from_bits(0x406ccccd),
            f32::from_bits(0x406ccccd),
            f32::from_bits(0x3e9a9fbe),
            f32::from_bits(0x3fbb22d1),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::circle(
            f32::from_bits(0xc06ccccd),
            f32::from_bits(0xc06ccccd),
            f32::from_bits(0x3e99999a),
            f32::from_bits(0x3f96c8b4),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::circle(
            f32::from_bits(0x406ccccd),
            f32::from_bits(0xc06ccccd),
            f32::from_bits(0x3e99999a),
            f32::from_bits(0x3f96c8b4),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::circle(
            f32::from_bits(0x00000000),
            f32::from_bits(0x4154cccd),
            f32::from_bits(0x3f0ae147),
            f32::from_bits(0x40654c98),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0xb373321e),
            f32::from_bits(0x412e0000),
            f32::from_bits(0x3f200000),
            f32::from_bits(0x3e000000),
            f32::from_bits(0xc0490fda),
            f32::from_bits(0x3f1645a2),
        ),
        behavior: PropBarrierBehavior::OneWayTop,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0xb2a22169),
            f32::from_bits(0x412a0000),
            f32::from_bits(0x3f200000),
            f32::from_bits(0x3e000000),
            f32::from_bits(0xc0490fda),
            f32::from_bits(0x3f3645a2),
        ),
        behavior: PropBarrierBehavior::OneWayTop,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0x32a22169),
            f32::from_bits(0x41260000),
            f32::from_bits(0x3f200000),
            f32::from_bits(0x3e000000),
            f32::from_bits(0xc0490fda),
            f32::from_bits(0x3f5645a2),
        ),
        behavior: PropBarrierBehavior::OneWayTop,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0x3373321e),
            f32::from_bits(0x41220000),
            f32::from_bits(0x3f200000),
            f32::from_bits(0x3e000000),
            f32::from_bits(0xc0490fda),
            f32::from_bits(0x3f7645a2),
        ),
        behavior: PropBarrierBehavior::OneWayTop,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0xc0bc0000),
            f32::from_bits(0xbfd9999a),
            f32::from_bits(0x3f000000),
            f32::from_bits(0x3e000000),
            f32::from_bits(0x3fc90fda),
            f32::from_bits(0x3f1645a2),
        ),
        behavior: PropBarrierBehavior::OneWayTop,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0xc0c40000),
            f32::from_bits(0xbfd9999a),
            f32::from_bits(0x3f000000),
            f32::from_bits(0x3e000000),
            f32::from_bits(0x3fc90fda),
            f32::from_bits(0x3f3645a2),
        ),
        behavior: PropBarrierBehavior::OneWayTop,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0xc0cc0000),
            f32::from_bits(0xbfd9999a),
            f32::from_bits(0x3f000000),
            f32::from_bits(0x3e000000),
            f32::from_bits(0x3fc90fda),
            f32::from_bits(0x3f5645a2),
        ),
        behavior: PropBarrierBehavior::OneWayTop,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0xc0d40000),
            f32::from_bits(0xbfd9999a),
            f32::from_bits(0x3f000000),
            f32::from_bits(0x3e000000),
            f32::from_bits(0x3fc90fda),
            f32::from_bits(0x3f7645a2),
        ),
        behavior: PropBarrierBehavior::OneWayTop,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0xc0bd999a),
            f32::from_bits(0x40200000),
            f32::from_bits(0x3f000000),
            f32::from_bits(0x3e000000),
            f32::from_bits(0x3fc90fda),
            f32::from_bits(0x3f1645a2),
        ),
        behavior: PropBarrierBehavior::OneWayTop,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0xc0c5999a),
            f32::from_bits(0x40200000),
            f32::from_bits(0x3f000000),
            f32::from_bits(0x3e000000),
            f32::from_bits(0x3fc90fda),
            f32::from_bits(0x3f3645a2),
        ),
        behavior: PropBarrierBehavior::OneWayTop,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0xc0cd999a),
            f32::from_bits(0x40200000),
            f32::from_bits(0x3f000000),
            f32::from_bits(0x3e000000),
            f32::from_bits(0x3fc90fda),
            f32::from_bits(0x3f5645a2),
        ),
        behavior: PropBarrierBehavior::OneWayTop,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0xc0d5999a),
            f32::from_bits(0x40200000),
            f32::from_bits(0x3f000000),
            f32::from_bits(0x3e000000),
            f32::from_bits(0x3fc90fda),
            f32::from_bits(0x3f7645a2),
        ),
        behavior: PropBarrierBehavior::OneWayTop,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0x40bc0000),
            f32::from_bits(0xbfd9999a),
            f32::from_bits(0x3f000000),
            f32::from_bits(0x3e000000),
            f32::from_bits(0xbfc90fda),
            f32::from_bits(0x3f1645a2),
        ),
        behavior: PropBarrierBehavior::OneWayTop,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0x40c40000),
            f32::from_bits(0xbfd9999a),
            f32::from_bits(0x3f000000),
            f32::from_bits(0x3e000000),
            f32::from_bits(0xbfc90fda),
            f32::from_bits(0x3f3645a2),
        ),
        behavior: PropBarrierBehavior::OneWayTop,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0x40cc0000),
            f32::from_bits(0xbfd9999a),
            f32::from_bits(0x3f000000),
            f32::from_bits(0x3e000000),
            f32::from_bits(0xbfc90fda),
            f32::from_bits(0x3f5645a2),
        ),
        behavior: PropBarrierBehavior::OneWayTop,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0x40d40000),
            f32::from_bits(0xbfd9999a),
            f32::from_bits(0x3f000000),
            f32::from_bits(0x3e000000),
            f32::from_bits(0xbfc90fda),
            f32::from_bits(0x3f7645a2),
        ),
        behavior: PropBarrierBehavior::OneWayTop,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0x40bd999a),
            f32::from_bits(0x40200000),
            f32::from_bits(0x3f000000),
            f32::from_bits(0x3e000000),
            f32::from_bits(0xbfc90fda),
            f32::from_bits(0x3f1645a2),
        ),
        behavior: PropBarrierBehavior::OneWayTop,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0x40c5999a),
            f32::from_bits(0x40200000),
            f32::from_bits(0x3f000000),
            f32::from_bits(0x3e000000),
            f32::from_bits(0xbfc90fda),
            f32::from_bits(0x3f3645a2),
        ),
        behavior: PropBarrierBehavior::OneWayTop,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0x40cd999a),
            f32::from_bits(0x40200000),
            f32::from_bits(0x3f000000),
            f32::from_bits(0x3e000000),
            f32::from_bits(0xbfc90fda),
            f32::from_bits(0x3f5645a2),
        ),
        behavior: PropBarrierBehavior::OneWayTop,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0x40d5999a),
            f32::from_bits(0x40200000),
            f32::from_bits(0x3f000000),
            f32::from_bits(0x3e000000),
            f32::from_bits(0xbfc90fda),
            f32::from_bits(0x3f7645a2),
        ),
        behavior: PropBarrierBehavior::OneWayTop,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0xc11e6666),
            f32::from_bits(0x00000000),
            f32::from_bits(0x3f000000),
            f32::from_bits(0x3e99999a),
            f32::from_bits(0x3fc90fda),
            f32::from_bits(0x4000c49c),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0x411e6666),
            f32::from_bits(0x00000000),
            f32::from_bits(0x3f000000),
            f32::from_bits(0x3e99999a),
            f32::from_bits(0xbfc90fda),
            f32::from_bits(0x4000c49c),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0xbefd2f1b),
            f32::from_bits(0xc1533333),
            f32::from_bits(0x3da4dd2f),
            f32::from_bits(0x3eb0a3d7),
            f32::from_bits(0x00000000),
            f32::from_bits(0x3fce5604),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0x3efd2f1b),
            f32::from_bits(0xc1533333),
            f32::from_bits(0x3da4dd2f),
            f32::from_bits(0x3eb0a3d7),
            f32::from_bits(0x00000000),
            f32::from_bits(0x3fce5604),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0x00000000),
            f32::from_bits(0xc1533333),
            f32::from_bits(0x3f133333),
            f32::from_bits(0x3eb0a3d7),
            f32::from_bits(0x00000000),
            f32::from_bits(0x3fce5604),
        ),
        behavior: PropBarrierBehavior::OneWayTop,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0x3efd2f1b),
            f32::from_bits(0x41800000),
            f32::from_bits(0x3da4dd2f),
            f32::from_bits(0x3eb0a3d7),
            f32::from_bits(0xc0490fda),
            f32::from_bits(0x3fce5604),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0xbefd2f1b),
            f32::from_bits(0x41800000),
            f32::from_bits(0x3da4dd2f),
            f32::from_bits(0x3eb0a3d7),
            f32::from_bits(0xc0490fda),
            f32::from_bits(0x3fce5604),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0x00000000),
            f32::from_bits(0x41800000),
            f32::from_bits(0x3f133333),
            f32::from_bits(0x3eb0a3d7),
            f32::from_bits(0xc0490fda),
            f32::from_bits(0x3fce5604),
        ),
        behavior: PropBarrierBehavior::OneWayTop,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0xc0c00000),
            f32::from_bits(0xc1533333),
            f32::from_bits(0x3f133333),
            f32::from_bits(0x3eb0a3d7),
            f32::from_bits(0x00000000),
            f32::from_bits(0x3fce5604),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0x40c00000),
            f32::from_bits(0xc1533333),
            f32::from_bits(0x3f133333),
            f32::from_bits(0x3eb0a3d7),
            f32::from_bits(0x00000000),
            f32::from_bits(0x3fce5604),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0xc0c00000),
            f32::from_bits(0x41800000),
            f32::from_bits(0x3f133333),
            f32::from_bits(0x3eb0a3d7),
            f32::from_bits(0xc0490fda),
            f32::from_bits(0x3fce5604),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0x40c00000),
            f32::from_bits(0x41800000),
            f32::from_bits(0x3f133333),
            f32::from_bits(0x3eb0a3d7),
            f32::from_bits(0xc0490fda),
            f32::from_bits(0x3fce5604),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0xc1500000),
            f32::from_bits(0x00000000),
            f32::from_bits(0x3f133333),
            f32::from_bits(0x3eb0a3d7),
            f32::from_bits(0x3fc90fda),
            f32::from_bits(0x3fce5604),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0x41500000),
            f32::from_bits(0x00000000),
            f32::from_bits(0x3f133333),
            f32::from_bits(0x3eb0a3d7),
            f32::from_bits(0xbfc90fda),
            f32::from_bits(0x3fce5604),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0xc1380d7d),
            f32::from_bits(0xc1375976),
            f32::from_bits(0x3eeb851f),
            f32::from_bits(0x3e30a3d7),
            f32::from_bits(0x3f490fdc),
            f32::from_bits(0x3fce5604),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0xc1318c1d),
            f32::from_bits(0xc1375976),
            f32::from_bits(0x3e30a3d7),
            f32::from_bits(0x3eeb851f),
            f32::from_bits(0x3f490fdc),
            f32::from_bits(0x3fce5604),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0x41357357),
            f32::from_bits(0xc134bf50),
            f32::from_bits(0x3eeb851f),
            f32::from_bits(0x3e30a3d7),
            f32::from_bits(0xbf490fdc),
            f32::from_bits(0x3fce5604),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0x41357357),
            f32::from_bits(0xc13b40b0),
            f32::from_bits(0x3e30a3d7),
            f32::from_bits(0x3eeb851f),
            f32::from_bits(0xbf490fdc),
            f32::from_bits(0x3fce5604),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0xc1357357),
            f32::from_bits(0x41498c1d),
            f32::from_bits(0x3eeb851f),
            f32::from_bits(0x3e30a3d7),
            f32::from_bits(0x4016cbe4),
            f32::from_bits(0x3fce5604),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0xc1357357),
            f32::from_bits(0x41500d7d),
            f32::from_bits(0x3e30a3d7),
            f32::from_bits(0x3eeb851f),
            f32::from_bits(0x4016cbe4),
            f32::from_bits(0x3fce5604),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0x41380d7d),
            f32::from_bits(0x414c2643),
            f32::from_bits(0x3eeb851f),
            f32::from_bits(0x3e30a3d7),
            f32::from_bits(0xc016cbe4),
            f32::from_bits(0x3fce5604),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0x41318c1d),
            f32::from_bits(0x414c2643),
            f32::from_bits(0x3e30a3d7),
            f32::from_bits(0x3eeb851f),
            f32::from_bits(0xc016cbe4),
            f32::from_bits(0x3fce5604),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0x00000000),
            f32::from_bits(0x40a66666),
            f32::from_bits(0x3fa66666),
            f32::from_bits(0x3e99999a),
            f32::from_bits(0x00000000),
            f32::from_bits(0x3f66e978),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0x00000000),
            f32::from_bits(0xc0a66666),
            f32::from_bits(0x3fa66666),
            f32::from_bits(0x3e99999a),
            f32::from_bits(0xc0490fda),
            f32::from_bits(0x3f66e978),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0xc0a66666),
            f32::from_bits(0x00000000),
            f32::from_bits(0x3fa66666),
            f32::from_bits(0x3e99999a),
            f32::from_bits(0x3fc90fda),
            f32::from_bits(0x3f66e978),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0x40a66666),
            f32::from_bits(0x00000000),
            f32::from_bits(0x3fa66666),
            f32::from_bits(0x3e99999a),
            f32::from_bits(0xbfc90fda),
            f32::from_bits(0x3f66e978),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0xc0966666),
            f32::from_bits(0x40980000),
            f32::from_bits(0x3ecccccd),
            f32::from_bits(0x3e19999a),
            f32::from_bits(0x00000000),
            f32::from_bits(0x3f66e978),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0xc08e6666),
            f32::from_bits(0x40900000),
            f32::from_bits(0x3e19999a),
            f32::from_bits(0x3ecccccd),
            f32::from_bits(0x00000000),
            f32::from_bits(0x3f66e978),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0x408e6666),
            f32::from_bits(0x40900000),
            f32::from_bits(0x3ecccccd),
            f32::from_bits(0x3e19999a),
            f32::from_bits(0x3fc90fda),
            f32::from_bits(0x3f66e978),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0x40966666),
            f32::from_bits(0x40980000),
            f32::from_bits(0x3e19999a),
            f32::from_bits(0x3ecccccd),
            f32::from_bits(0x3fc90fda),
            f32::from_bits(0x3f66e978),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0x40966666),
            f32::from_bits(0xc0980000),
            f32::from_bits(0x3ecccccd),
            f32::from_bits(0x3e19999a),
            f32::from_bits(0xc0490fda),
            f32::from_bits(0x3f66e978),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0x408e6666),
            f32::from_bits(0xc0900000),
            f32::from_bits(0x3e19999a),
            f32::from_bits(0x3ecccccd),
            f32::from_bits(0xc0490fda),
            f32::from_bits(0x3f66e978),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0xc08e6666),
            f32::from_bits(0xc0900000),
            f32::from_bits(0x3ecccccd),
            f32::from_bits(0x3e19999a),
            f32::from_bits(0xbfc90fda),
            f32::from_bits(0x3f66e978),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0xc0966666),
            f32::from_bits(0xc0980000),
            f32::from_bits(0x3e19999a),
            f32::from_bits(0x3ecccccd),
            f32::from_bits(0xbfc90fda),
            f32::from_bits(0x3f66e978),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::circle(
            f32::from_bits(0xc1433333),
            f32::from_bits(0xc10e6666),
            f32::from_bits(0x3e199999),
            f32::from_bits(0x3fa7ef9e),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::circle(
            f32::from_bits(0x41433333),
            f32::from_bits(0xc10e6666),
            f32::from_bits(0x3e199999),
            f32::from_bits(0x3fa7ef9e),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::circle(
            f32::from_bits(0xc12ccccd),
            f32::from_bits(0x412ccccd),
            f32::from_bits(0x3e072b02),
            f32::from_bits(0x3f9ae148),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::circle(
            f32::from_bits(0x412ccccd),
            f32::from_bits(0x412ccccd),
            f32::from_bits(0x3e072b02),
            f32::from_bits(0x3f9ae148),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::circle(
            f32::from_bits(0xc1266666),
            f32::from_bits(0x40733333),
            f32::from_bits(0x3dc49ba6),
            f32::from_bits(0x3fda5e35),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::circle(
            f32::from_bits(0x41266666),
            f32::from_bits(0x40733333),
            f32::from_bits(0x3dc49ba6),
            f32::from_bits(0x3fda5e35),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::circle(
            f32::from_bits(0xc0bccccd),
            f32::from_bits(0xc0b33333),
            f32::from_bits(0x3e828f5d),
            f32::from_bits(0x3f89096c),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::circle(
            f32::from_bits(0x40c00000),
            f32::from_bits(0xc0a33333),
            f32::from_bits(0x3e7be76d),
            f32::from_bits(0x3f864990),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0xc0b9999a),
            f32::from_bits(0x40cccccd),
            f32::from_bits(0x3eb1de6a),
            f32::from_bits(0x3ec2eb1c),
            f32::from_bits(0x3e97e9d7),
            f32::from_bits(0x3f553261),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0x40e00000),
            f32::from_bits(0x40866666),
            f32::from_bits(0x3e9e1b09),
            f32::from_bits(0x3ead42c4),
            f32::from_bits(0xbe567750),
            f32::from_bits(0x3f4aa64c),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0x00000000),
            f32::from_bits(0xc1245a1c),
            f32::from_bits(0x3ec7ae14),
            f32::from_bits(0x3e851eb8),
            f32::from_bits(0x00000000),
            f32::from_bits(0x3fa624dd),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0xc0951597),
            f32::from_bits(0xc140b71d),
            f32::from_bits(0x3ec7ae14),
            f32::from_bits(0x3e666666),
            f32::from_bits(0x3edf66f4),
            f32::from_bits(0x3f6dd2f2),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0x4093581f),
            f32::from_bits(0xc1404f41),
            f32::from_bits(0x3ec7ae14),
            f32::from_bits(0x3e666666),
            f32::from_bits(0xbedf66f4),
            f32::from_bits(0x3f6dd2f2),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0xc0f654b8),
            f32::from_bits(0x41554d4d),
            f32::from_bits(0x3ea9ba5e),
            f32::from_bits(0x3e43d70a),
            f32::from_bits(0x4032b8c2),
            f32::from_bits(0x3f5be426),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0x40f7dd50),
            f32::from_bits(0x415594c0),
            f32::from_bits(0x3ea9ba5e),
            f32::from_bits(0x3e43d70a),
            f32::from_bits(0xc032b8c2),
            f32::from_bits(0x3f5be426),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0xc14ccccd),
            f32::from_bits(0x40cccccd),
            f32::from_bits(0x3f000000),
            f32::from_bits(0x3e99999a),
            f32::from_bits(0x3fc90fda),
            f32::from_bits(0x3fbb22d1),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0x414ccccd),
            f32::from_bits(0x40cccccd),
            f32::from_bits(0x3f000000),
            f32::from_bits(0x3e99999a),
            f32::from_bits(0xbfc90fda),
            f32::from_bits(0x3fbb22d1),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0xc06ccccd),
            f32::from_bits(0xc1526666),
            f32::from_bits(0x3f000000),
            f32::from_bits(0x3e99999a),
            f32::from_bits(0x00000000),
            f32::from_bits(0x3fbb22d1),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0x406ccccd),
            f32::from_bits(0xc1526666),
            f32::from_bits(0x3f000000),
            f32::from_bits(0x3e99999a),
            f32::from_bits(0x00000000),
            f32::from_bits(0x3fbb22d1),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0xc06ccccd),
            f32::from_bits(0x417e6666),
            f32::from_bits(0x3f000000),
            f32::from_bits(0x3e99999a),
            f32::from_bits(0xc0490fda),
            f32::from_bits(0x3fbb22d1),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0x406ccccd),
            f32::from_bits(0x417e6666),
            f32::from_bits(0x3f000000),
            f32::from_bits(0x3e99999a),
            f32::from_bits(0xc0490fda),
            f32::from_bits(0x3fbb22d1),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0xc109999a),
            f32::from_bits(0xc0d9999a),
            f32::from_bits(0x3ec5a1cb),
            f32::from_bits(0x3ed89375),
            f32::from_bits(0x3f567751),
            f32::from_bits(0x3f5fbe76),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0xc104344d),
            f32::from_bits(0xc0ed192d),
            f32::from_bits(0x3ecccccd),
            f32::from_bits(0x3ecccccd),
            f32::from_bits(0x3fa31564),
            f32::from_bits(0x3f5cac08),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0xc10b69bf),
            f32::from_bits(0xc0c784df),
            f32::from_bits(0x3ea66666),
            f32::from_bits(0x3ea66666),
            f32::from_bits(0x3ea0d97a),
            f32::from_bits(0x3f4978d4),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0x41066666),
            f32::from_bits(0xc0d00000),
            f32::from_bits(0x3eb1de6a),
            f32::from_bits(0x3ec2eb1c),
            f32::from_bits(0xbec49809),
            f32::from_bits(0x3f553261),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0x41104e55),
            f32::from_bits(0xc0cce019),
            f32::from_bits(0x3eb851eb),
            f32::from_bits(0x3eb851eb),
            f32::from_bits(0x3d567756),
            f32::from_bits(0x3f526e97),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0x40fc646e),
            f32::from_bits(0xc0cd8045),
            f32::from_bits(0x3e95c28f),
            f32::from_bits(0x3e95c28f),
            f32::from_bits(0xbf685695),
            f32::from_bits(0x3f4126e9),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0xc02ccccd),
            f32::from_bits(0x41000000),
            f32::from_bits(0x3e943958),
            f32::from_bits(0x3ea26e98),
            f32::from_bits(0x3eb2b8c3),
            f32::from_bits(0x3f456042),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0xc010c6d8),
            f32::from_bits(0x40f6e340),
            f32::from_bits(0x3e99999a),
            f32::from_bits(0x3e99999a),
            f32::from_bits(0x3f490fdc),
            f32::from_bits(0x3f43126e),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
    WorldPropBarrier {
        definition: ArenaBarrierDefinition::rectangle(
            f32::from_bits(0xc03e55d5),
            f32::from_bits(0x4105592c),
            f32::from_bits(0x3e799999),
            f32::from_bits(0x3e799999),
            f32::from_bits(0xbe32b8c3),
            f32::from_bits(0x3f34ac08),
        ),
        behavior: PropBarrierBehavior::Solid,
    },
];

#[cfg(test)]
const CHAMPIONS_COURT_COLLISION_FNV1A64: u64 = 0x16273c63e5b838fc;

fn champions_court_collision_barriers() -> &'static [WorldPropBarrier] {
    &CHAMPIONS_COURT_COLLISION_BARRIERS
}

#[cfg(test)]
fn append_champions_object_barriers(
    assets: &HashMap<String, String>,
    object: &ChampionsCourtObject,
    transform: Transform,
    barriers: &mut Vec<WorldPropBarrier>,
) {
    let Some(asset) = assets.get(&object.asset) else {
        return;
    };
    let (yaw, _, _) = transform.rotation.to_euler(EulerRot::YXZ);
    barriers.extend(
        prop_collision_profile(asset)
            .iter()
            .copied()
            .map(|barrier| barrier.to_world_scaled(transform.translation, yaw, transform.scale)),
    );
}

/// Immutable collision data derived from the geometry rendered for one arena.
///
/// Prop profiles are authored in model-local space. Converting them to world space
/// requires scale and rotation work that used to happen for every ground and side
/// collision probe. The arena catalog is static, so the converted barriers can be
/// built once and safely shared by fighters, bots, and tests.
#[allow(dead_code)]
pub struct ArenaCollisionWorld {
    arena_index: usize,
    prop_barriers: Vec<WorldPropBarrier>,
}

#[allow(dead_code)]
impl ArenaCollisionWorld {
    pub fn arena_index(&self) -> usize {
        self.arena_index
    }

    pub fn prop_barrier_count(&self) -> usize {
        self.prop_barriers.len()
    }
}

fn arena_collision_worlds() -> &'static [ArenaCollisionWorld] {
    static WORLDS: OnceLock<Vec<ArenaCollisionWorld>> = OnceLock::new();
    WORLDS.get_or_init(|| {
        arena_definitions()
            .iter()
            .enumerate()
            .map(|(arena_index, arena)| {
                let mut prop_barriers: Vec<_> = arena_asset_props_for_definition(arena)
                    .iter()
                    .copied()
                    .flat_map(ArenaAssetProp::collision_barriers)
                    .collect();
                if arena.visual_theme == ArenaVisualTheme::Crown {
                    prop_barriers.extend(champions_court_collision_barriers().iter().copied());
                }
                ArenaCollisionWorld {
                    arena_index,
                    prop_barriers,
                }
            })
            .collect()
    })
}

pub fn arena_collision_world(arena: &ArenaDefinition) -> &'static ArenaCollisionWorld {
    let arena_index = arena_definitions()
        .iter()
        .position(|candidate| std::ptr::eq(candidate, arena))
        .or_else(|| {
            arena_definitions()
                .iter()
                .position(|candidate| candidate.name == arena.name)
        })
        .unwrap_or(CHAMPIONS_COURT_ARENA_INDEX);
    &arena_collision_worlds()[arena_index]
}

fn arena_prop_barriers(arena: &ArenaDefinition) -> impl Iterator<Item = WorldPropBarrier> + '_ {
    arena_collision_world(arena).prop_barriers.iter().copied()
}

const CROWN_ASSET_PROPS: &[ArenaAssetProp] = &[
    ArenaAssetProp {
        name: "Crown north statue",
        file: "statue.glb",
        x: -2.2,
        y: ARENA_TOP_Y,
        z: 8.85,
        yaw: PI,
        scale: 1.65,
    },
    ArenaAssetProp {
        name: "Crown south statue",
        file: "statue.glb",
        x: 2.2,
        y: ARENA_TOP_Y,
        z: -8.85,
        yaw: 0.0,
        scale: 1.65,
    },
    ArenaAssetProp {
        name: "Crown rear banner left",
        file: "banner.glb",
        x: -5.7,
        y: ARENA_TOP_Y,
        z: -8.9,
        yaw: 0.0,
        scale: 1.55,
    },
    ArenaAssetProp {
        name: "Crown rear banner right",
        file: "banner.glb",
        x: 5.7,
        y: ARENA_TOP_Y,
        z: -8.9,
        yaw: 0.0,
        scale: 1.55,
    },
    ArenaAssetProp {
        name: "Crown prize trophy",
        file: "trophy.glb",
        x: 0.0,
        y: ARENA_TOP_Y + 0.12,
        z: 9.8,
        yaw: PI,
        scale: 1.45,
    },
];

const SPLIT_ASSET_PROPS: &[ArenaAssetProp] = &[
    ArenaAssetProp {
        name: "Split west bridge frame",
        file: "tower/wood-structure-high.glb",
        x: -7.2,
        y: ARENA_TOP_Y,
        z: 0.0,
        yaw: PI * 0.5,
        scale: 2.4,
    },
    ArenaAssetProp {
        name: "Split east bridge frame",
        file: "tower/wood-structure-high.glb",
        x: 7.2,
        y: ARENA_TOP_Y,
        z: 0.0,
        yaw: PI * 0.5,
        scale: 2.4,
    },
    ArenaAssetProp {
        name: "Split west watch tree",
        file: "tower/detail-tree-large.glb",
        x: -8.4,
        y: ARENA_TOP_Y - 0.15,
        z: 5.6,
        yaw: 0.25,
        scale: 2.25,
    },
    ArenaAssetProp {
        name: "Split east watch tree",
        file: "tower/detail-tree-large.glb",
        x: 8.4,
        y: ARENA_TOP_Y - 0.15,
        z: -5.6,
        yaw: -0.35,
        scale: 2.0,
    },
];

const SUNSTONE_ASSET_PROPS: &[ArenaAssetProp] = &[
    ArenaAssetProp {
        name: "Sunstone west timber lookout",
        file: "tower/wood-structure.glb",
        x: -6.0,
        y: ARENA_TOP_Y + 0.08,
        z: 3.8,
        yaw: -0.55,
        scale: 2.4,
    },
    ArenaAssetProp {
        name: "Sunstone east timber lookout",
        file: "tower/wood-structure.glb",
        x: 6.0,
        y: ARENA_TOP_Y + 0.08,
        z: -3.8,
        yaw: -0.55,
        scale: 2.4,
    },
    ArenaAssetProp {
        name: "Sunstone west rocks",
        file: "tower/detail-rocks-large.glb",
        x: -6.0,
        y: ARENA_TOP_Y,
        z: -4.7,
        yaw: 0.2,
        scale: 2.2,
    },
    ArenaAssetProp {
        name: "Sunstone east rocks",
        file: "tower/detail-rocks-large.glb",
        x: 6.0,
        y: ARENA_TOP_Y,
        z: 4.7,
        yaw: -0.25,
        scale: 2.2,
    },
];

const CRANK_ASSET_PROPS: &[ArenaAssetProp] = &[
    ArenaAssetProp {
        name: "Crank yard west conveyor",
        file: "platformer/conveyor-belt.glb",
        x: -3.15,
        y: ARENA_TOP_Y + 0.01,
        z: 0.0,
        yaw: PI * 0.5,
        scale: 2.4,
    },
    ArenaAssetProp {
        name: "Crank yard east conveyor",
        file: "platformer/conveyor-belt.glb",
        x: 3.15,
        y: ARENA_TOP_Y + 0.01,
        z: 0.0,
        yaw: -PI * 0.5,
        scale: 2.4,
    },
    ArenaAssetProp {
        name: "Crank yard north pipe",
        file: "platformer/pipe.glb",
        x: -1.7,
        y: ARENA_TOP_Y,
        z: 7.0,
        yaw: PI,
        scale: CRANK_PIPE_VISUAL_SCALE,
    },
    ArenaAssetProp {
        name: "Crank yard south pipe",
        file: "platformer/pipe.glb",
        x: 1.7,
        y: ARENA_TOP_Y,
        z: -7.0,
        yaw: 0.0,
        scale: CRANK_PIPE_VISUAL_SCALE,
    },
];

const VENT_SPIRAL_ASSET_PROPS: &[ArenaAssetProp] = &[ArenaAssetProp {
    name: "Vent spiral crystal core",
    file: "tower/tower-round-crystals.glb",
    x: 0.0,
    y: crate::arena_defs::VENT_SPIRAL_REACTOR_BASE_Y,
    z: 0.0,
    yaw: crate::arena_defs::VENT_SPIRAL_REACTOR_YAW,
    scale: crate::arena_defs::VENT_SPIRAL_REACTOR_SCALE,
}];

const BUMPER_ALLEY_ASSET_PROPS: &[ArenaAssetProp] = &[
    ArenaAssetProp {
        name: "Bumper alley north spring",
        file: "platformer/spring.glb",
        x: 0.0,
        y: ARENA_TOP_Y + 0.02,
        z: 4.25,
        yaw: 0.0,
        scale: 2.2,
    },
    ArenaAssetProp {
        name: "Bumper alley center spring",
        file: "platformer/spring.glb",
        x: 0.0,
        y: ARENA_TOP_Y + 0.02,
        z: 0.0,
        yaw: PI * 0.5,
        scale: 2.2,
    },
    ArenaAssetProp {
        name: "Bumper alley south spring",
        file: "platformer/spring.glb",
        x: 0.0,
        y: ARENA_TOP_Y + 0.02,
        z: -4.25,
        yaw: PI,
        scale: 2.2,
    },
    ArenaAssetProp {
        name: "Bumper alley west target",
        file: "blaster/target-large.glb",
        x: -4.15,
        y: ARENA_TOP_Y + 0.03,
        z: 7.6,
        yaw: PI * 0.5,
        scale: 2.5,
    },
    ArenaAssetProp {
        name: "Bumper alley east target",
        file: "blaster/target-large.glb",
        x: 4.15,
        y: ARENA_TOP_Y + 0.03,
        z: -7.6,
        yaw: -PI * 0.5,
        scale: 2.5,
    },
];

const FEAST_MARKET_ASSET_PROPS: &[ArenaAssetProp] = &[
    ArenaAssetProp {
        name: "Feast market burger stall",
        file: "food/burger-cheese-double.glb",
        x: -5.8,
        y: ARENA_TOP_Y + 0.02,
        z: 3.4,
        yaw: 0.25,
        scale: 3.2,
    },
    ArenaAssetProp {
        name: "Feast market cake stall",
        file: "food/cake.glb",
        x: 5.8,
        y: ARENA_TOP_Y + 0.02,
        z: -3.4,
        yaw: -0.25,
        scale: 3.3,
    },
    ArenaAssetProp {
        name: "Feast market pizza sign",
        file: "food/pizza.glb",
        x: 3.0,
        y: ARENA_TOP_Y + 0.05,
        z: 6.2,
        yaw: 0.1,
        scale: 3.3,
    },
    ArenaAssetProp {
        name: "Feast market watermelon stand",
        file: "food/watermelon.glb",
        x: -3.0,
        y: ARENA_TOP_Y + 0.02,
        z: -6.2,
        yaw: -0.15,
        scale: 3.0,
    },
    ArenaAssetProp {
        name: "Feast market stew pot",
        file: "food/pot-stew.glb",
        x: 5.7,
        y: ARENA_TOP_Y + 0.02,
        z: 3.9,
        yaw: 0.3,
        scale: 3.0,
    },
    ArenaAssetProp {
        name: "Feast market supply crate",
        file: "platformer/crate.glb",
        x: -5.9,
        y: ARENA_TOP_Y,
        z: -3.9,
        yaw: 0.2,
        scale: 1.8,
    },
];

const SNARE_GARDEN_ASSET_PROPS: &[ArenaAssetProp] = &[
    ArenaAssetProp {
        name: "Snare garden west flowers",
        file: "platformer/flowers-tall.glb",
        x: -5.1,
        y: ARENA_TOP_Y + 0.02,
        z: 1.7,
        yaw: 0.35,
        scale: 2.1,
    },
    ArenaAssetProp {
        name: "Snare garden east flowers",
        file: "platformer/flowers.glb",
        x: 5.1,
        y: ARENA_TOP_Y + 0.02,
        z: -1.7,
        yaw: -0.4,
        scale: 2.2,
    },
    ArenaAssetProp {
        name: "Snare garden old tree",
        file: "platformer/tree.glb",
        x: -7.8,
        y: ARENA_TOP_Y - 0.08,
        z: 6.8,
        yaw: 0.2,
        scale: 2.4,
    },
];

const SKY_STEPS_ASSET_PROPS: &[ArenaAssetProp] = &[
    ArenaAssetProp {
        name: "Sky steps west pine",
        file: "platformer/tree-pine-snow.glb",
        x: -8.0,
        y: ARENA_TOP_Y - 0.18,
        z: -6.4,
        yaw: 0.1,
        scale: 1.8,
    },
    ArenaAssetProp {
        name: "Sky steps east pine",
        file: "platformer/tree-pine-snow-small.glb",
        x: 6.8,
        y: ARENA_TOP_Y + 0.8,
        z: 5.5,
        yaw: -0.2,
        scale: 2.4,
    },
    ArenaAssetProp {
        name: "Sky steps snowman",
        file: "holiday/snowman.glb",
        x: -5.8,
        y: ARENA_TOP_Y + 0.28,
        z: 4.7,
        yaw: 0.35,
        scale: 2.0,
    },
    ArenaAssetProp {
        name: "Sky steps signal lantern",
        file: "holiday/lantern.glb",
        x: 5.6,
        y: ARENA_TOP_Y + 0.31,
        z: -4.7,
        yaw: 0.0,
        scale: 1.8,
    },
    ArenaAssetProp {
        name: "Sky steps timber shelter",
        file: "tower/snow-wood-structure.glb",
        x: 0.0,
        y: ARENA_TOP_Y + 0.38,
        z: 0.0,
        yaw: PI * 0.25,
        scale: 1.8,
    },
    ArenaAssetProp {
        name: "Sky steps west snow bank",
        file: "holiday/snow-pile.glb",
        x: -3.0,
        y: ARENA_TOP_Y + 0.18,
        z: -2.4,
        yaw: 0.1,
        scale: 2.6,
    },
    ArenaAssetProp {
        name: "Sky steps east snow bank",
        file: "tower/snow-detail-rocks-large.glb",
        x: 3.0,
        y: ARENA_TOP_Y + 0.58,
        z: 2.4,
        yaw: -0.15,
        scale: 2.4,
    },
];

const POWDER_KEG_ASSET_PROPS: &[ArenaAssetProp] = &[
    ArenaAssetProp {
        name: "Powder keg west cannon",
        file: "tower/weapon-cannon.glb",
        x: -6.7,
        y: ARENA_TOP_Y,
        z: 1.8,
        yaw: PI * 0.5,
        scale: 2.5,
    },
    ArenaAssetProp {
        name: "Powder keg east cannon",
        file: "tower/weapon-cannon.glb",
        x: 6.7,
        y: ARENA_TOP_Y,
        z: -1.8,
        yaw: -PI * 0.75,
        scale: 2.5,
    },
    ArenaAssetProp {
        name: "Powder keg catapult",
        file: "tower/weapon-catapult.glb",
        x: 0.0,
        y: ARENA_TOP_Y,
        z: 6.8,
        yaw: PI,
        scale: 2.3,
    },
    ArenaAssetProp {
        name: "Powder keg bomb cache",
        file: "platformer/bomb.glb",
        x: -3.8,
        y: ARENA_TOP_Y + 0.02,
        z: -5.8,
        yaw: 0.25,
        scale: 2.3,
    },
    ArenaAssetProp {
        name: "Powder keg barrel cache",
        file: "platformer/barrel.glb",
        x: 3.8,
        y: ARENA_TOP_Y,
        z: 5.8,
        yaw: -0.25,
        scale: 2.1,
    },
    ArenaAssetProp {
        name: "Powder keg cannonballs",
        file: "tower/weapon-ammo-cannonball.glb",
        x: 5.4,
        y: ARENA_TOP_Y + 0.02,
        z: 5.6,
        yaw: 0.0,
        scale: 2.5,
    },
];

fn spawn_arena_lights(commands: &mut Commands) {
    commands.spawn((
        DirectionalLight {
            illuminance: 12_500.0,
            shadows_enabled: true,
            ..default()
        },
        Transform::from_xyz(-5.0, 12.0, 7.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));

    commands.spawn((
        PointLight {
            intensity: 1_100_000.0,
            range: 36.0,
            shadows_enabled: false,
            ..default()
        },
        Transform::from_xyz(0.0, 9.0, 4.5),
    ));
}

fn spawn_campfire_props(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    hazards: &[ArenaHazardDefinition],
) {
    if !hazards
        .iter()
        .any(|hazard| hazard.kind == ArenaHazardKind::Campfire)
    {
        return;
    }

    let stone_mesh = meshes.add(Cuboid::new(0.34, 0.2, 0.27));
    let log_mesh = meshes.add(Cylinder::new(0.12, 1.05));
    let outer_flame_mesh = meshes.add(Cone::new(0.38, 0.95));
    let inner_flame_mesh = meshes.add(Cone::new(0.2, 0.58));
    let stone_material = materials.add(StandardMaterial {
        base_color: Color::srgb(0.32, 0.29, 0.26),
        perceptual_roughness: 0.96,
        ..default()
    });
    let log_material = materials.add(StandardMaterial {
        base_color: Color::srgb(0.24, 0.09, 0.035),
        perceptual_roughness: 0.92,
        ..default()
    });
    let outer_flame_material = materials.add(StandardMaterial {
        base_color: Color::srgb(1.0, 0.19, 0.025),
        emissive: LinearRgba::from(Color::srgb(1.0, 0.08, 0.01)) * 5.0,
        perceptual_roughness: 0.48,
        ..default()
    });
    let inner_flame_material = materials.add(StandardMaterial {
        base_color: Color::srgb(1.0, 0.76, 0.08),
        emissive: LinearRgba::from(Color::srgb(1.0, 0.48, 0.025)) * 7.0,
        perceptual_roughness: 0.42,
        ..default()
    });

    for hazard in hazards
        .iter()
        .filter(|hazard| hazard.kind == ArenaHazardKind::Campfire)
    {
        for stone_index in 0..8 {
            let angle = stone_index as f32 / 8.0 * TAU;
            commands.spawn((
                Mesh3d(stone_mesh.clone()),
                MeshMaterial3d(stone_material.clone()),
                Transform::from_xyz(
                    hazard.center.x + angle.cos() * 0.62,
                    hazard.center.y + 0.1,
                    hazard.center.z + angle.sin() * 0.62,
                )
                .with_rotation(Quat::from_rotation_y(-angle)),
                Name::new("Campfire stone"),
                ArenaGeometry,
            ));
        }

        for yaw in [PI * 0.25, -PI * 0.25] {
            commands.spawn((
                Mesh3d(log_mesh.clone()),
                MeshMaterial3d(log_material.clone()),
                Transform::from_xyz(hazard.center.x, hazard.center.y + 0.24, hazard.center.z)
                    .with_rotation(Quat::from_rotation_y(yaw) * Quat::from_rotation_z(PI * 0.5)),
                Name::new("Campfire log"),
                ArenaGeometry,
            ));
        }

        let outer_scale = Vec3::new(1.0, 1.0, 1.0);
        commands.spawn((
            Mesh3d(outer_flame_mesh.clone()),
            MeshMaterial3d(outer_flame_material.clone()),
            Transform::from_xyz(hazard.center.x, hazard.center.y + 0.63, hazard.center.z)
                .with_scale(outer_scale),
            ArenaCampfireFlame {
                base_scale: outer_scale,
                phase: hazard.phase,
            },
            Name::new("Campfire outer flame"),
            ArenaGeometry,
        ));

        let inner_scale = Vec3::new(0.92, 1.0, 0.92);
        commands.spawn((
            Mesh3d(inner_flame_mesh.clone()),
            MeshMaterial3d(inner_flame_material.clone()),
            Transform::from_xyz(
                hazard.center.x,
                hazard.center.y + 0.48,
                hazard.center.z - 0.03,
            )
            .with_scale(inner_scale),
            ArenaCampfireFlame {
                base_scale: inner_scale,
                phase: hazard.phase + 1.7,
            },
            Name::new("Campfire inner flame"),
            ArenaGeometry,
        ));

        commands.spawn((
            PointLight {
                color: Color::srgb(1.0, 0.32, 0.06),
                intensity: 180_000.0,
                range: 4.5,
                shadows_enabled: false,
                ..default()
            },
            Transform::from_xyz(hazard.center.x, hazard.center.y + 1.05, hazard.center.z),
            Name::new("Campfire light"),
            ArenaGeometry,
        ));
    }
}

#[allow(dead_code)]
fn spawn_pipe_portal_visuals(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    pipe_pair: Option<ArenaPipePairDefinition>,
) {
    let Some(pipe_pair) = pipe_pair else {
        return;
    };

    let ring_mesh = meshes.add(Torus::new(0.66, 0.055));
    let particle_mesh = meshes.add(Sphere::new(0.075).mesh().uv(8, 5));
    let portal_material = materials.add(StandardMaterial {
        base_color: Color::srgba(0.12, 1.0, 0.72, 0.82),
        emissive: LinearRgba::from(Color::srgb(0.04, 1.0, 0.58)) * 5.0,
        alpha_mode: AlphaMode::Blend,
        perceptual_roughness: 0.32,
        ..default()
    });

    for (endpoint, center) in pipe_pair.endpoints.into_iter().enumerate() {
        let base_scale = Vec3::splat(1.0);
        commands.spawn((
            Mesh3d(ring_mesh.clone()),
            MeshMaterial3d(portal_material.clone()),
            Transform::from_xyz(center.x, pipe_pair.top_y + 0.045, center.y).with_scale(base_scale),
            ArenaPipePortalRing {
                endpoint,
                phase: endpoint as f32 * PI,
                base_scale,
            },
            Name::new(format!("Crank pipe portal ring {endpoint}")),
            ArenaGeometry,
        ));

        for particle_index in 0..5 {
            let phase = particle_index as f32 / 5.0 * TAU + endpoint as f32 * 0.8;
            let radius = 0.32 + (particle_index % 2) as f32 * 0.16;
            commands.spawn((
                Mesh3d(particle_mesh.clone()),
                MeshMaterial3d(portal_material.clone()),
                Transform::from_xyz(
                    center.x + phase.cos() * radius,
                    pipe_pair.top_y + 0.12,
                    center.y + phase.sin() * radius,
                ),
                ArenaPipePortalParticle {
                    endpoint,
                    phase,
                    radius,
                    base_y: pipe_pair.top_y + 0.08,
                },
                Name::new("Crank pipe portal mote"),
                ArenaGeometry,
            ));
        }

        commands.spawn((
            PointLight {
                color: Color::srgb(0.08, 1.0, 0.62),
                intensity: 70_000.0,
                range: 3.0,
                shadows_enabled: false,
                ..default()
            },
            Transform::from_xyz(center.x, pipe_pair.top_y + 0.55, center.y),
            Name::new("Crank pipe portal light"),
            ArenaGeometry,
        ));
    }
}

fn spawn_crank_yard_machinery(
    commands: &mut Commands,
    asset_server: &AssetServer,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    arena_index: usize,
    hazards: &[ArenaHazardDefinition],
) {
    if arena_index != CRANK_YARD_ARENA_INDEX {
        return;
    }

    let running_rotation = Quat::from_rotation_y(-PI * 0.5);
    commands.spawn((
        SceneRoot(asset_server.load(
            GltfAssetLabel::Scene(0).from_asset(arena_prop_asset_path("platformer/lever.glb")),
        )),
        Transform::from_translation(CRANK_LEVER_POSITION)
            .with_rotation(running_rotation)
            .with_scale(Vec3::splat(2.0)),
        CrankLeverVisual {
            running_rotation,
            stopped_rotation: running_rotation * Quat::from_rotation_z(-0.82),
        },
        Name::new("Crank yard saw stop lever"),
        ArenaGeometry,
    ));

    let saw_scene = asset_server
        .load(GltfAssetLabel::Scene(0).from_asset(arena_prop_asset_path("platformer/saw.glb")));
    let housing_mesh = meshes.add(Cuboid::new(1.75, 0.28, 0.2));
    let light_mesh = meshes.add(Sphere::new(0.13).mesh().uv(10, 6));
    let spark_mesh = meshes.add(Sphere::new(0.045).mesh().uv(6, 4));
    let housing_material = materials.add(StandardMaterial {
        base_color: Color::srgb(0.1, 0.12, 0.14),
        metallic: 0.72,
        perceptual_roughness: 0.38,
        ..default()
    });
    let warning_material = materials.add(StandardMaterial {
        base_color: Color::srgb(1.0, 0.035, 0.015),
        emissive: LinearRgba::from(Color::srgb(1.0, 0.015, 0.005)) * 7.0,
        perceptual_roughness: 0.25,
        ..default()
    });
    let spark_material = materials.add(StandardMaterial {
        base_color: Color::srgb(1.0, 0.72, 0.08),
        emissive: LinearRgba::from(Color::srgb(1.0, 0.36, 0.015)) * 6.0,
        perceptual_roughness: 0.3,
        ..default()
    });

    for (index, hazard) in hazards
        .iter()
        .filter(|hazard| hazard.kind == ArenaHazardKind::SawBlade)
        .enumerate()
    {
        let spin_speed = if index % 2 == 0 { 9.5 } else { -9.5 };
        commands.spawn((
            SceneRoot(saw_scene.clone()),
            Transform::from_xyz(hazard.center.x, CRANK_SAW_VISUAL_Y, hazard.center.z)
                .with_scale(Vec3::splat(2.5)),
            ArenaSawBladeVisual { spin_speed },
            Name::new(format!("Crank yard active saw {index}")),
            ArenaGeometry,
        ));

        for side in [-1.0, 1.0] {
            commands.spawn((
                Mesh3d(housing_mesh.clone()),
                MeshMaterial3d(housing_material.clone()),
                Transform::from_xyz(
                    hazard.center.x,
                    ARENA_TOP_Y + 0.5,
                    hazard.center.z + side * 0.68,
                ),
                Name::new("Crank saw housing rail"),
                ArenaGeometry,
            ));
        }

        let warning_scale = Vec3::splat(1.35);
        commands.spawn((
            Mesh3d(light_mesh.clone()),
            MeshMaterial3d(warning_material.clone()),
            Transform::from_xyz(hazard.center.x, ARENA_TOP_Y + 1.22, hazard.center.z + 0.72)
                .with_scale(warning_scale),
            ArenaSawWarningLight {
                phase: index as f32 * PI,
                base_scale: warning_scale,
            },
            Name::new("Crank saw warning lamp"),
            ArenaGeometry,
        ));

        for spark_index in 0..5 {
            commands.spawn((
                Mesh3d(spark_mesh.clone()),
                MeshMaterial3d(spark_material.clone()),
                Transform::from_xyz(hazard.center.x, ARENA_TOP_Y + 0.92, hazard.center.z),
                ArenaSawAmbientSpark {
                    center: Vec3::new(hazard.center.x, ARENA_TOP_Y + 0.92, hazard.center.z),
                    phase: spark_index as f32 / 5.0 * TAU + index as f32,
                },
                Name::new("Crank saw tooth spark"),
                ArenaGeometry,
            ));
        }
    }
}

fn spawn_vent_spiral_machinery(
    commands: &mut Commands,
    asset_server: &AssetServer,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    arena_index: usize,
    hazards: &[ArenaHazardDefinition],
) {
    if arena_index != VENT_SPIRAL_ARENA_INDEX {
        return;
    }

    let housing_material = materials.add(StandardMaterial {
        base_color: Color::srgb(0.08, 0.11, 0.12),
        metallic: 0.68,
        perceptual_roughness: 0.36,
        ..default()
    });
    let rotor_material = materials.add(StandardMaterial {
        base_color: Color::srgb(0.58, 0.66, 0.64),
        metallic: 0.82,
        perceptual_roughness: 0.26,
        ..default()
    });
    let warning_material = materials.add(StandardMaterial {
        base_color: Color::srgb(1.0, 0.34, 0.06),
        emissive: LinearRgba::rgb(1.6, 0.18, 0.01),
        metallic: 0.08,
        perceptual_roughness: 0.42,
        ..default()
    });
    let plume_material = materials.add(StandardMaterial {
        base_color: Color::srgba(0.28, 0.96, 0.88, 0.48),
        emissive: LinearRgba::rgb(0.16, 1.3, 1.0),
        alpha_mode: AlphaMode::Blend,
        cull_mode: None,
        perceptual_roughness: 0.22,
        ..default()
    });
    let blade_mesh = meshes.add(Cuboid::new(0.5, 0.055, 0.16));
    let hub_mesh = meshes.add(Cylinder::new(0.15, 0.1));
    let warning_bulb_mesh = meshes.add(Sphere::new(0.065).mesh().uv(8, 5));
    let plume_mesh = meshes.add(Cone::new(0.22, 1.0));

    for (index, hazard) in hazards
        .iter()
        .filter(|hazard| hazard.kind == ArenaHazardKind::PulseVent)
        .enumerate()
    {
        commands.spawn((
            Mesh3d(meshes.add(Cylinder::new(hazard.radius * 0.93, 0.18))),
            MeshMaterial3d(housing_material.clone()),
            Transform::from_xyz(hazard.center.x, hazard.center.y + 0.03, hazard.center.z),
            Name::new(format!("Vent spiral turbine housing {index}")),
            ArenaGeometry,
        ));

        commands
            .spawn((
                Transform::from_xyz(hazard.center.x, hazard.center.y + 0.14, hazard.center.z),
                Visibility::Visible,
                ArenaVentRotor {
                    pulse_seconds: hazard.pulse_seconds,
                    phase: hazard.phase,
                    spin_direction: if index % 2 == 0 { 1.0 } else { -1.0 },
                },
                Name::new(format!("Vent spiral turbine rotor {index}")),
                ArenaGeometry,
            ))
            .with_children(|parent| {
                for blade_index in 0..5 {
                    let angle = blade_index as f32 / 5.0 * TAU;
                    parent.spawn((
                        Mesh3d(blade_mesh.clone()),
                        MeshMaterial3d(rotor_material.clone()),
                        Transform::from_xyz(angle.cos() * 0.27, 0.0, angle.sin() * 0.27)
                            .with_rotation(Quat::from_rotation_y(-angle)),
                        Name::new("Vent turbine fan blade"),
                    ));
                }
                parent.spawn((
                    Mesh3d(hub_mesh.clone()),
                    MeshMaterial3d(warning_material.clone()),
                    Transform::from_xyz(0.0, 0.055, 0.0),
                    Name::new("Vent turbine energy hub"),
                ));
            });

        let warning_scale = Vec3::splat(1.0);
        commands.spawn((
            Mesh3d(meshes.add(Annulus::new(hazard.radius * 0.98, hazard.radius * 1.14))),
            MeshMaterial3d(warning_material.clone()),
            Transform::from_xyz(hazard.center.x, hazard.center.y + 0.155, hazard.center.z)
                .with_rotation(Quat::from_rotation_x(-PI * 0.5)),
            ArenaVentWarning {
                pulse_seconds: hazard.pulse_seconds,
                phase: hazard.phase,
                base_scale: warning_scale,
            },
            Name::new(format!("Vent spiral warning ring {index}")),
            ArenaGeometry,
        ));

        for bulb_index in 0..8 {
            let angle = bulb_index as f32 / 8.0 * TAU;
            let radius = hazard.radius * 1.08;
            let base_scale = Vec3::splat(if bulb_index % 2 == 0 { 1.0 } else { 0.76 });
            commands.spawn((
                Mesh3d(warning_bulb_mesh.clone()),
                MeshMaterial3d(warning_material.clone()),
                Transform::from_xyz(
                    hazard.center.x + angle.cos() * radius,
                    hazard.center.y + 0.19,
                    hazard.center.z + angle.sin() * radius,
                )
                .with_scale(base_scale),
                ArenaVentWarning {
                    pulse_seconds: hazard.pulse_seconds,
                    phase: hazard.phase + bulb_index as f32 * 0.025,
                    base_scale,
                },
                Name::new("Vent turbine warning lamp"),
                ArenaGeometry,
            ));
        }

        for plume_index in 0..3 {
            let angle = plume_index as f32 / 3.0 * TAU + index as f32 * 0.7;
            let base_y = hazard.center.y + 0.2;
            let full_height = 1.65 + plume_index as f32 * 0.18;
            let base_scale = Vec3::new(0.76, full_height, 0.76);
            commands.spawn((
                Mesh3d(plume_mesh.clone()),
                MeshMaterial3d(plume_material.clone()),
                Transform::from_xyz(
                    hazard.center.x + angle.cos() * 0.2,
                    base_y,
                    hazard.center.z + angle.sin() * 0.2,
                )
                .with_scale(Vec3::new(base_scale.x, 0.001, base_scale.z)),
                ArenaVentPlume {
                    pulse_seconds: hazard.pulse_seconds,
                    phase: hazard.phase + plume_index as f32 * 0.035,
                    base_y,
                    full_height,
                    base_scale,
                },
                Name::new("Vent turbine energy plume"),
                ArenaGeometry,
            ));
        }
    }

    let ufo_position = Vec3::new(5.8, ARENA_TOP_Y + 4.15, -7.0);
    commands.spawn((
        SceneRoot(asset_server.load(
            GltfAssetLabel::Scene(0).from_asset(arena_prop_asset_path("tower/enemy-ufo-a.glb")),
        )),
        Transform::from_translation(ufo_position)
            .with_rotation(Quat::from_rotation_y(0.25))
            .with_scale(Vec3::splat(2.2)),
        ArenaVentUfo {
            base_y: ufo_position.y,
        },
        Name::new("Vent spiral background ufo"),
        ArenaGeometry,
    ));

    let beam_scale = Vec3::new(1.75, 4.0, 1.75);
    commands.spawn((
        SceneRoot(asset_server.load(
            GltfAssetLabel::Scene(0).from_asset(arena_prop_asset_path("tower/enemy-ufo-beam.glb")),
        )),
        Transform::from_xyz(5.8, ARENA_TOP_Y + 0.08, -7.0).with_scale(beam_scale),
        ArenaVentUfoBeam {
            base_y: ARENA_TOP_Y + 0.08,
            base_scale: beam_scale,
        },
        Name::new("Vent spiral background ufo beam"),
        ArenaGeometry,
    ));
}

fn spawn_arena_hazard_markers(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    material: Handle<StandardMaterial>,
    arena_index: usize,
    hazards: &[ArenaHazardDefinition],
) {
    for hazard in hazards.iter().filter(|hazard| {
        hazard.kind != ArenaHazardKind::SawBlade
            && !(arena_index == VENT_SPIRAL_ARENA_INDEX
                && hazard.kind == ArenaHazardKind::PulseVent)
    }) {
        let base_scale = (hazard.pulse_seconds / 2.2).clamp(0.8, 1.3);
        commands.spawn((
            Mesh3d(meshes.add(Annulus::new(hazard.radius * 0.68, hazard.radius))),
            MeshMaterial3d(material.clone()),
            Transform::from_translation(hazard.center)
                .with_rotation(Quat::from_rotation_x(-PI * 0.5))
                .with_scale(Vec3::splat(base_scale)),
            ArenaHazardMarker {
                kind: hazard.kind,
                pulse_seconds: hazard.pulse_seconds,
                phase: hazard.phase,
                base_scale,
                base_y: hazard.center.y,
            },
            ArenaGeometry,
        ));
    }
}

pub fn update_arena_hazard_visuals(
    state: Res<ArenaHazardState>,
    mut markers: Query<(&ArenaHazardMarker, &mut Transform), Without<ArenaCampfireFlame>>,
    mut flames: Query<(&ArenaCampfireFlame, &mut Transform), Without<ArenaHazardMarker>>,
) {
    let elapsed = state.elapsed();
    for (marker, mut transform) in &mut markers {
        let wave = ((elapsed + marker.phase) / marker.pulse_seconds.max(0.1) * TAU).sin();
        let scale = marker.base_scale * arena_hazard_marker_scale(marker.kind, wave);
        transform.scale = Vec3::new(scale, marker.base_scale, scale);
        transform.translation.y = marker.base_y
            + if marker.kind == ArenaHazardKind::BumperNode {
                wave.max(0.0) * 0.14
            } else {
                0.0
            };
    }

    for (flame, mut transform) in &mut flames {
        let flicker = (elapsed * 9.0 + flame.phase).sin();
        let flutter = (elapsed * 13.0 + flame.phase * 0.7).sin();
        transform.scale = flame.base_scale
            * Vec3::new(
                1.0 - flicker * 0.055,
                1.0 + flicker * 0.1,
                1.0 + flutter * 0.045,
            );
    }
}

pub fn update_arena_pipe_visuals(
    time: Res<Time>,
    active_arena: Res<ActiveArena>,
    state: Res<ArenaPipeState>,
    mut rings: Query<(&ArenaPipePortalRing, &mut Transform), Without<ArenaPipePortalParticle>>,
    mut particles: Query<(&ArenaPipePortalParticle, &mut Transform), Without<ArenaPipePortalRing>>,
) {
    let Some(pipe_pair) = active_arena.definition().pipe_pair else {
        return;
    };
    let elapsed = time.elapsed_secs();

    for (ring, mut transform) in &mut rings {
        let active = state.endpoint_active(ring.endpoint);
        let pulse = (elapsed * if active { 8.0 } else { 3.2 } + ring.phase).sin();
        let scale = 1.0 + pulse * 0.06 + if active { 0.16 } else { 0.0 };
        transform.scale = ring.base_scale * scale;
        transform.rotate_y(time.delta_secs() * if active { 2.8 } else { 1.1 });
    }

    for (particle, mut transform) in &mut particles {
        let Some(center) = pipe_pair.endpoints.get(particle.endpoint).copied() else {
            continue;
        };
        let active = state.endpoint_active(particle.endpoint);
        let speed = if active { 3.8 } else { 1.65 };
        let angle = elapsed * speed + particle.phase;
        let rise =
            (elapsed * if active { 1.9 } else { 1.15 } + particle.phase / TAU).rem_euclid(1.0);
        transform.translation = Vec3::new(
            center.x + angle.cos() * particle.radius,
            particle.base_y + rise * if active { 1.05 } else { 0.62 },
            center.y + angle.sin() * particle.radius,
        );
        transform.scale = Vec3::splat((1.0 - rise).max(0.08) * if active { 1.35 } else { 1.0 });
    }
}

pub fn update_crank_yard_machinery(
    active_arena: Res<ActiveArena>,
    mut state: ResMut<ArenaHazardState>,
    fighters: Query<(&FighterInput, &SimPosition), With<Fighter>>,
) {
    let arena_index = active_arena.index();
    state.sync_to_arena(arena_index, active_arena.definition().hazards.len());
    state.crank_lever_toggle_cooldown.tick();

    if arena_index == CRANK_YARD_ARENA_INDEX
        && !state.crank_lever_toggle_cooldown.active()
        && fighters.iter().any(|(input, transform)| {
            (input.raw_light_pressed || input.raw_heavy_pressed)
                && crate::canonical_math::vec2_length_squared(Vec2::new(
                    transform.translation.x - CRANK_LEVER_POSITION.x,
                    transform.translation.z - CRANK_LEVER_POSITION.z,
                )) <= CRANK_LEVER_ATTACK_RADIUS * CRANK_LEVER_ATTACK_RADIUS
        })
    {
        state.crank_saws_stopped = !state.crank_saws_stopped;
        state.crank_lever_toggle_cooldown = TickTimer::from_millis_ceil(300);
    }
}

/// Animates the crank-yard presentation from canonical device state. This is
/// deliberately render-rate work and is never installed in the headless app.
pub fn update_crank_yard_machinery_visuals(
    time: Res<Time>,
    state: Res<ArenaHazardState>,
    mut levers: Query<
        (&CrankLeverVisual, &mut Transform),
        (
            Without<Fighter>,
            Without<ArenaSawBladeVisual>,
            Without<ArenaSawWarningLight>,
            Without<ArenaSawAmbientSpark>,
        ),
    >,
    mut blades: Query<
        (&ArenaSawBladeVisual, &mut Transform),
        (
            Without<Fighter>,
            Without<CrankLeverVisual>,
            Without<ArenaSawWarningLight>,
            Without<ArenaSawAmbientSpark>,
        ),
    >,
    mut warning_lights: Query<
        (&ArenaSawWarningLight, &mut Transform),
        (
            Without<Fighter>,
            Without<CrankLeverVisual>,
            Without<ArenaSawBladeVisual>,
            Without<ArenaSawAmbientSpark>,
        ),
    >,
    mut sparks: Query<
        (&ArenaSawAmbientSpark, &mut Transform),
        (
            Without<Fighter>,
            Without<CrankLeverVisual>,
            Without<ArenaSawBladeVisual>,
            Without<ArenaSawWarningLight>,
        ),
    >,
) {
    let dt = time.delta_secs();
    let elapsed = state.elapsed();

    for (lever, mut transform) in &mut levers {
        let target = if state.crank_saws_stopped {
            lever.stopped_rotation
        } else {
            lever.running_rotation
        };
        transform.rotation = transform.rotation.slerp(target, (dt * 9.0).min(1.0));
    }

    for (blade, mut transform) in &mut blades {
        if !state.crank_saws_stopped {
            transform.rotate_local_z(blade.spin_speed * dt);
        }
    }

    for (warning, mut transform) in &mut warning_lights {
        let pulse = ((elapsed * 7.0 + warning.phase).sin() * 0.5 + 0.5).powf(3.0);
        transform.scale = warning.base_scale
            * if state.crank_saws_stopped {
                0.28
            } else {
                0.72 + pulse * 0.5
            };
    }

    for (spark, mut transform) in &mut sparks {
        let cycle = elapsed * 2.7 + spark.phase;
        let flare = (cycle.sin() * 0.5 + 0.5).powf(9.0);
        let angle = cycle * 2.3;
        transform.translation = spark.center
            + Vec3::new(
                angle.cos() * (0.46 + flare * 0.28),
                flare * 0.52,
                angle.sin() * 0.34,
            );
        transform.scale = Vec3::splat(if state.crank_saws_stopped {
            0.0
        } else {
            flare * 1.6
        });
    }
}

/// Reconciles visual bomb children from authoritative ordnance entities. No
/// spawn event is needed: state reconciliation also handles snapshot restore,
/// late join, and corrected despawns without replaying a one-shot.
pub fn sync_arena_cannon_bomb_visuals(
    mut commands: Commands,
    time: Res<Time>,
    assets: Res<ArenaOrdnanceAssets>,
    mut bombs: Query<
        (
            Entity,
            &SimPosition,
            Option<&mut Transform>,
            Option<&Visibility>,
        ),
        (With<ArenaCannonBomb>, Without<ArenaCannonBombVisual>),
    >,
    existing_visuals: Query<&ChildOf, With<ArenaCannonBombVisual>>,
    mut visual_transforms: Query<
        &mut Transform,
        (With<ArenaCannonBombVisual>, Without<ArenaCannonBomb>),
    >,
) {
    for (bomb_entity, position, transform, visibility) in &mut bombs {
        if let Some(mut transform) = transform {
            transform.translation = position.translation;
        } else {
            commands
                .entity(bomb_entity)
                .insert(Transform::from_translation(position.translation));
        }
        if visibility.is_none() {
            // The authoritative bomb is a non-rendering hierarchy root. Its
            // mesh child still inherits visibility, so the parent must own the
            // ordinary visibility components before the child is attached.
            commands.entity(bomb_entity).insert(Visibility::Inherited);
        }
        if existing_visuals
            .iter()
            .any(|parent| parent.parent() == bomb_entity)
        {
            continue;
        }
        commands.spawn((
            Mesh3d(assets.bomb_mesh.clone()),
            MeshMaterial3d(assets.bomb_material.clone()),
            Transform::default(),
            ArenaCannonBombVisual,
            ChildOf(bomb_entity),
            Name::new("Powder keg cannon bomb visual"),
        ));
    }

    let dt = time.delta_secs();
    for mut transform in &mut visual_transforms {
        transform.rotate_x(dt * 8.0);
        transform.rotate_z(dt * 5.0);
    }
}

pub(crate) fn present_arena_impact_accent(
    commands: &mut Commands,
    effect_assets: &EffectAssets,
    fighter_entity: Entity,
    intent: ArenaPresentationIntent,
) {
    match intent.accent {
        ArenaImpactAccent::None => {}
        ArenaImpactAccent::CampfireBurn { duration_seconds } => {
            commands
                .entity(fighter_entity)
                .insert(ArenaFighterBurn::new(duration_seconds));
            spawn_burning_fighter_effect(commands, effect_assets, fighter_entity, duration_seconds);
        }
        ArenaImpactAccent::MachineScratch { position } => {
            spawn_machine_scratch(commands, effect_assets, fighter_entity, position);
        }
    }
}

/// Advances the render-local burn tint. The authoritative hazard cooldown and
/// damage live in fixed simulation; this component exists only to shade the
/// rendered fighter and is recreated from the stable impact event on rollback.
pub fn update_arena_fighter_burns(
    time: Res<Time>,
    mut commands: Commands,
    mut burns: Query<(Entity, &mut ArenaFighterBurn)>,
) {
    let dt = time.delta_secs();
    for (fighter_entity, mut burn) in &mut burns {
        burn.remaining_seconds = (burn.remaining_seconds - dt).max(0.0);
        if burn.remaining_seconds <= 0.0 {
            commands.entity(fighter_entity).remove::<ArenaFighterBurn>();
        }
    }
}

pub fn advance_powder_keg_cannons_and_collect_contacts(
    active_arena: Res<ActiveArena>,
    mut stable_entities: StableEntityCommands,
    tick: Res<SimTick>,
    mut cannon_state: ResMut<PowderKegCannonState>,
    match_state: Res<MatchState>,
    feel: Res<CombatFeelTuning>,
    hitstop: Res<Hitstop>,
    mut contact_frame: ResMut<ArenaOrdnanceContactFrame>,
    mut contact_buffer: ResMut<ContactBuffer>,
    mut bombs: Query<(&StableSimEntity, &mut ArenaCannonBomb, &mut SimPosition)>,
    fighters: Query<
        (&Fighter, &FighterStats, &FighterActionState, &SimPosition),
        Without<ArenaCannonBomb>,
    >,
) {
    if hitstop.active() {
        contact_frame.cancel_tick();
        return;
    }
    contact_frame.begin_tick(*tick);

    let arena_index = active_arena.index();
    cannon_state.sync_to_arena(arena_index);
    if arena_index != POWDER_KEG_ARENA_INDEX {
        // Arena visual reconciliation runs in render-rate `Update` and must
        // never own authoritative ordnance lifetime. Clear bombs here, through
        // the stable allocator, when a fighting match changes away from Powder
        // Keg; match reset performs the same canonical cleanup explicitly.
        let mut stale_ordnance = [None; ARENA_ORDNANCE_ENTITY_CAPACITY];
        let mut stale_len = 0;
        for index in 0..stable_entities
            .identities
            .capacity(SimEntityKind::ArenaOrdnance)
        {
            let Some((stable_id, entity)) = stable_entities
                .identities
                .entry_at(SimEntityKind::ArenaOrdnance, index)
            else {
                continue;
            };
            let Ok((stable, _, _)) = bombs.get_mut(entity) else {
                continue;
            };
            if stable.id() == stable_id {
                if stale_len < stale_ordnance.len() {
                    stale_ordnance[stale_len] = Some((entity, *stable));
                    stale_len += 1;
                }
            }
        }
        for (entity, stable) in stale_ordnance[..stale_len].iter().flatten().copied() {
            despawn_stable(
                &mut stable_entities.commands,
                &mut stable_entities.identities,
                entity,
                stable,
            );
        }
        return;
    }

    if cannon_state.fire_timer.tick() {
        let (origin, velocity) = powder_cannon_shot(cannon_state.next_cannon);
        let _ = try_spawn_stable(
            &mut stable_entities.commands,
            &mut stable_entities.identities,
            SimEntityKind::ArenaOrdnance,
            (
                SimPosition::new(origin),
                ArenaCannonBomb {
                    velocity,
                    lifetime: TickTimer::from_millis_ceil(3_400),
                },
            ),
        );
        cannon_state.next_cannon = (cannon_state.next_cannon + 1) % 2;
        cannon_state.fire_timer = TickTimer::from_millis_ceil(2_600);
    }

    let arena = active_arena.definition();
    for index in 0..stable_entities
        .identities
        .capacity(SimEntityKind::ArenaOrdnance)
    {
        let Some((stable_id, bomb_entity)) = stable_entities
            .identities
            .entry_at(SimEntityKind::ArenaOrdnance, index)
        else {
            continue;
        };
        let Ok((stable, mut bomb, mut bomb_transform)) = bombs.get_mut(bomb_entity) else {
            continue;
        };
        if stable.id() != stable_id {
            continue;
        }
        let expired = bomb.lifetime.tick();
        bomb.velocity.y -= GRAVITY * SIM_DT_SECONDS;
        bomb_transform.translation += bomb.velocity * SIM_DT_SECONDS;

        let position = bomb_transform.translation;
        let ground_hit = ground_support_for_arena_with_radius(arena, position.x, position.z, 0.0)
            .height()
            .is_some_and(|ground_y| position.y <= ground_y + 0.22 && bomb.velocity.y <= 0.0);
        let mut detonated = ground_hit || expired;

        for victim in FighterId::ALL {
            let Some((fighter, stats, action, transform)) = fighters
                .iter()
                .find(|(fighter, ..)| fighter.id == victim.index())
            else {
                continue;
            };
            if !match_state.fighter_can_participate(fighter.id)
                || !can_receive_impact(&stats, &action)
            {
                continue;
            }
            let fighter_center = transform.translation + Vec3::Y * 0.72;
            let hit_radius = if ground_hit || expired {
                POWDER_CANNON_BOMB_RADIUS
            } else {
                0.34 + FIGHTER_RADIUS * stats.item_size_multiplier()
            };
            debug_assert!(hit_radius >= 0.0);
            if crate::canonical_math::vec3_distance_squared(fighter_center, position)
                > hit_radius * hit_radius
            {
                continue;
            }
            let mut impact =
                powder_cannon_impact_profile().with_hit_effects_enabled(feel.hit_effects_enabled());
            impact.knockback_direction = Some(crate::canonical_math::vec3_normalize_or(
                Vec3::new(
                    transform.translation.x - position.x,
                    0.0,
                    transform.translation.z - position.z,
                ),
                Vec3::Z,
            ));
            let _ = contact_buffer.push(ContactRecord::new(
                ContactPhase::Strike,
                ContactSourceKind::ArenaOrdnance,
                stable_id,
                None,
                victim,
                u16::MAX,
                crate::techniques::AttackShapeId::BombBurst as u16,
                0,
                fighter_center,
                position,
                impact,
                ContactFlags::default(),
            ));
            detonated = true;
        }

        if detonated {
            contact_frame.record_detonation(stable_id);
        }
    }
}

/// Despawns cannon sources only after every frozen target has been resolved.
/// Impact presentation and semantic hit events are emitted centrally by the
/// contact resolver, so this consumer emits no additional event ordinal.
pub fn apply_powder_keg_contact_outcomes(
    mut commands: Commands,
    mut identities: ResMut<crate::ecs_identity::SimulationIdentityAllocator>,
    tick: Res<SimTick>,
    contact_frame: Res<ArenaOrdnanceContactFrame>,
    bombs: Query<(&StableSimEntity, &ArenaCannonBomb)>,
) {
    if contact_frame.tick != Some(*tick) {
        return;
    }
    for source in contact_frame.detonations() {
        let Some(entity) = identities.mapped_entity(source) else {
            continue;
        };
        let Ok((stable, _)) = bombs.get(entity) else {
            continue;
        };
        if stable.id() != source {
            continue;
        }
        despawn_stable(&mut commands, &mut identities, entity, *stable);
    }
}

fn powder_cannon_shot(index: usize) -> (Vec3, Vec3) {
    let cannon = if index % 2 == 0 {
        Vec3::new(-6.7, ARENA_TOP_Y + 1.05, 1.8)
    } else {
        Vec3::new(6.7, ARENA_TOP_Y + 1.05, -1.8)
    };
    let direction =
        crate::canonical_math::vec3_normalize_or(Vec3::new(-cannon.x, 0.0, -cannon.z), Vec3::X);
    (cannon + direction * 1.0, direction * 7.8 + Vec3::Y * 5.0)
}

fn powder_cannon_impact_profile() -> ImpactProfile {
    let mut profile = impact_profile(
        NEUTRAL_IMPACT_OWNER_ID,
        ImpactSource::Hazard,
        POWDER_CANNON_BOMB_DAMAGE,
        8.4,
        4.2,
        true,
        true,
        18.0,
        ImpactFeedbackIntensity::Heavy,
        ReactionFamilyId::LauncherDown,
    );
    profile.element = DamageElement::Hazard;
    profile
}

pub fn update_vent_spiral_machinery(
    time: Res<Time>,
    active_arena: Res<ActiveArena>,
    state: Res<ArenaHazardState>,
    mut visuals: ParamSet<(
        Query<(&ArenaVentRotor, &mut Transform)>,
        Query<(&ArenaVentWarning, &mut Transform)>,
        Query<(&ArenaVentPlume, &mut Transform)>,
        Query<(&ArenaVentUfo, &mut Transform)>,
        Query<(&ArenaVentUfoBeam, &mut Transform)>,
    )>,
) {
    let dt = time.delta_secs();
    let elapsed = state.elapsed();

    for (rotor, mut transform) in &mut visuals.p0() {
        let active = vent_active_visual_amount(elapsed, rotor.pulse_seconds, rotor.phase);
        let charge = vent_charge_visual_amount(elapsed, rotor.pulse_seconds, rotor.phase);
        transform.rotate_y(rotor.spin_direction * (2.2 + charge * 4.0 + active * 14.0) * dt);
    }

    for (warning, mut transform) in &mut visuals.p1() {
        let active = vent_active_visual_amount(elapsed, warning.pulse_seconds, warning.phase);
        let charge = vent_charge_visual_amount(elapsed, warning.pulse_seconds, warning.phase);
        let pulse = (elapsed * 15.0 + warning.phase * 3.0).sin().abs();
        transform.scale = warning.base_scale
            * (0.72 + charge * (0.32 + pulse * 0.2) + active * (0.35 + pulse * 0.16));
    }

    for (plume, mut transform) in &mut visuals.p2() {
        let active = vent_active_visual_amount(elapsed, plume.pulse_seconds, plume.phase);
        let flutter = (elapsed * 18.0 + plume.phase * 5.0).sin() * 0.06;
        let height_amount = (active + flutter * active).clamp(0.001, 1.0);
        transform.translation.y = plume.base_y + plume.full_height * height_amount * 0.5;
        transform.scale = Vec3::new(
            plume.base_scale.x * (0.58 + active * 0.42),
            plume.base_scale.y * height_amount,
            plume.base_scale.z * (0.58 + active * 0.42),
        );
    }

    for (ufo, mut transform) in &mut visuals.p3() {
        transform.translation.y = ufo.base_y + (elapsed * 1.7).sin() * 0.16;
        transform.rotate_y(dt * 0.42);
    }

    let sequence_amount = active_arena
        .definition()
        .hazards
        .iter()
        .filter(|hazard| hazard.kind == ArenaHazardKind::PulseVent)
        .map(|hazard| {
            vent_active_visual_amount(elapsed, hazard.pulse_seconds, hazard.phase)
                + vent_charge_visual_amount(elapsed, hazard.pulse_seconds, hazard.phase) * 0.28
        })
        .fold(0.0_f32, f32::max)
        .clamp(0.0, 1.0);
    for (beam, mut transform) in &mut visuals.p4() {
        let shimmer = (elapsed * 8.0).sin() * 0.04;
        let amount = (0.58 + sequence_amount * 0.42 + shimmer).clamp(0.5, 1.05);
        transform.translation.y = beam.base_y;
        transform.scale = Vec3::new(
            beam.base_scale.x * amount,
            beam.base_scale.y * (0.94 + sequence_amount * 0.06),
            beam.base_scale.z * amount,
        );
    }
}

fn vent_cycle_progress(elapsed: f32, pulse_seconds: f32, phase: f32) -> f32 {
    (elapsed + phase).rem_euclid(pulse_seconds.max(0.1)) / pulse_seconds.max(0.1)
}

fn vent_active_visual_amount(elapsed: f32, pulse_seconds: f32, phase: f32) -> f32 {
    let progress = vent_cycle_progress(elapsed, pulse_seconds, phase);
    let active_fraction = arena_hazard_active_fraction(ArenaHazardKind::PulseVent);
    if progress > active_fraction {
        return 0.0;
    }
    let active_progress = progress / active_fraction;
    0.25 + (active_progress * PI).sin().max(0.0) * 0.75
}

fn vent_charge_visual_amount(elapsed: f32, pulse_seconds: f32, phase: f32) -> f32 {
    let progress = vent_cycle_progress(elapsed, pulse_seconds, phase);
    ((progress - 0.72) / 0.28).clamp(0.0, 1.0)
}

fn fighter_pipe_base_scale(stats: &FighterStats) -> Vec3 {
    Vec3::splat(stats.item_size_multiplier())
}

pub fn update_arena_pipe_transits(
    active_arena: Res<ActiveArena>,
    match_state: Res<MatchState>,
    mut state: ResMut<ArenaPipeState>,
    mut fighters: ParamSet<(
        Query<(&Fighter, &SimPosition)>,
        Query<(
            &Fighter,
            &mut FighterStats,
            &mut FighterMotor,
            &mut FighterActionState,
            &mut SimPosition,
        )>,
    )>,
) {
    let arena_index = active_arena.index();
    state.sync_to_arena(arena_index);
    let Some(pipe_pair) = active_arena.definition().pipe_pair else {
        return;
    };
    let snapshots: ArrayVec<(usize, Vec3), FIGHTER_COUNT> = fighters
        .p0()
        .iter()
        .filter(|(fighter, _)| match_state.fighter_can_participate(fighter.id))
        .map(|(fighter, transform)| (fighter.id, transform.translation))
        .collect();

    for (fighter, mut stats, mut motor, mut action, mut transform) in &mut fighters.p1() {
        if fighter.id >= FIGHTER_COUNT || !match_state.fighter_can_participate(fighter.id) {
            continue;
        }

        match state.fighters[fighter.id] {
            FighterPipeState::Ready {
                candidate,
                dwell_ticks,
                mut cooldown,
            } => {
                cooldown.tick();
                let endpoint = if !cooldown.active() {
                    pipe_entry_endpoint(pipe_pair, transform.translation, &motor, action.action)
                } else {
                    None
                };
                let descending_entry = endpoint.is_some()
                    && !motor.grounded
                    && action.action == FighterAction::Jumping
                    && motor.velocity.y <= 0.0;
                let next_dwell_ticks = if descending_entry {
                    PIPE_ENTRY_DWELL_TICKS
                } else if endpoint.is_some() && endpoint == candidate {
                    dwell_ticks.saturating_add(1)
                } else {
                    0
                };

                if let Some(source) = endpoint
                    && next_dwell_ticks >= PIPE_ENTRY_DWELL_TICKS
                {
                    let destination = 1 - source;
                    state.fighters[fighter.id] = FighterPipeState::Transit {
                        source,
                        destination,
                        elapsed: ElapsedTicks::ZERO,
                        entry_y: transform.translation.y,
                        // Root scale is presentation-owned and may contain a
                        // render-time pulse. Derive the gameplay transit scale
                        // only from canonical status so render cadence cannot
                        // enter pipe state or rollback snapshots.
                        base_scale: fighter_pipe_base_scale(&stats),
                    };
                    motor.velocity = Vec3::ZERO;
                    motor.grounded = false;
                    *action = FighterActionState::default();
                    action.action = FighterAction::Respawning;
                    stats
                        .invulnerability
                        .set_max(TickTimer::from_seconds_ceil(0.25));
                } else {
                    state.fighters[fighter.id] = FighterPipeState::Ready {
                        candidate: endpoint,
                        dwell_ticks: next_dwell_ticks,
                        cooldown,
                    };
                }
            }
            FighterPipeState::Transit {
                source,
                destination,
                elapsed,
                entry_y,
                base_scale,
            } => {
                let destination_center = pipe_pair.endpoints[destination];
                let exit_occupied = snapshots.iter().any(|(other_id, position)| {
                    *other_id != fighter.id
                        && crate::canonical_math::vec2_length_squared(Vec2::new(
                            position.x - destination_center.x,
                            position.z - destination_center.y,
                        )) < PIPE_EXIT_CLEARANCE_RADIUS * PIPE_EXIT_CLEARANCE_RADIUS
                        && position.y >= pipe_pair.top_y - 0.2
                });
                let next_elapsed = if exit_occupied && elapsed.get() >= PIPE_HIDDEN_END_TICKS {
                    ElapsedTicks::from_ticks(PIPE_HIDDEN_END_TICKS)
                } else {
                    let mut elapsed = elapsed;
                    elapsed.advance();
                    elapsed
                };
                let sample = pipe_transit_sample(
                    pipe_pair,
                    source,
                    destination,
                    next_elapsed,
                    entry_y,
                    base_scale,
                );

                transform.translation = sample.position;
                motor.velocity = Vec3::ZERO;
                motor.grounded = false;
                *action = FighterActionState::default();
                action.action = FighterAction::Respawning;
                stats
                    .invulnerability
                    .set_max(TickTimer::from_seconds_ceil(0.25));

                if sample.complete {
                    transform.translation = Vec3::new(
                        destination_center.x,
                        pipe_pair.top_y + 0.06,
                        destination_center.y,
                    );
                    motor.facing = crate::canonical_math::vec3_normalize_or_zero(Vec3::new(
                        -destination_center.x,
                        0.0,
                        -destination_center.y,
                    ));
                    motor.velocity = motor.facing * PIPE_EXIT_INWARD_SPEED;
                    motor.velocity.y = PIPE_EXIT_HOP_SPEED;
                    motor.grounded = false;
                    *action = FighterActionState::default();
                    action.action = FighterAction::Jumping;
                    stats
                        .invulnerability
                        .set_max(TickTimer::from_seconds_ceil(0.35));
                    state.fighters[fighter.id] = FighterPipeState::Ready {
                        candidate: None,
                        dwell_ticks: 0,
                        cooldown: TickTimer::from_millis_ceil(900),
                    };
                } else {
                    state.fighters[fighter.id] = FighterPipeState::Transit {
                        source,
                        destination,
                        elapsed: next_elapsed,
                        entry_y,
                        base_scale,
                    };
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct PipeTransitSample {
    position: Vec3,
    scale: Vec3,
    complete: bool,
}

fn pipe_entry_endpoint(
    pipe_pair: ArenaPipePairDefinition,
    position: Vec3,
    motor: &FighterMotor,
    action: FighterAction,
) -> Option<usize> {
    let grounded_entry = motor.grounded
        && matches!(action, FighterAction::Idle | FighterAction::Moving)
        && (position.y - pipe_pair.top_y).abs() <= 0.18;
    let descending_entry = !motor.grounded
        && action == FighterAction::Jumping
        && motor.velocity.y <= 0.0
        && position.y >= pipe_pair.top_y - 0.12
        && position.y <= pipe_pair.top_y + 0.58;
    if !grounded_entry && !descending_entry {
        return None;
    }

    pipe_pair.endpoints.iter().position(|center| {
        debug_assert!(pipe_pair.trigger_radius >= 0.0);
        crate::canonical_math::vec2_length_squared(Vec2::new(
            position.x - center.x,
            position.z - center.y,
        )) <= pipe_pair.trigger_radius * pipe_pair.trigger_radius
    })
}

fn pipe_transit_sample(
    pipe_pair: ArenaPipePairDefinition,
    source: usize,
    destination: usize,
    elapsed: ElapsedTicks,
    entry_y: f32,
    base_scale: Vec3,
) -> PipeTransitSample {
    let source_center = pipe_pair.endpoints[source];
    let destination_center = pipe_pair.endpoints[destination];
    let hidden_y = pipe_pair.top_y - PIPE_SINK_DEPTH;
    let elapsed_seconds = elapsed.as_seconds();

    if elapsed.get() < PIPE_ENTER_END_TICKS {
        let t = smooth_step(elapsed_seconds / PIPE_ENTER_SECONDS);
        return PipeTransitSample {
            position: Vec3::new(
                source_center.x,
                entry_y + (hidden_y - entry_y) * t,
                source_center.y,
            ),
            scale: base_scale * (1.0 + (0.45 - 1.0) * t),
            complete: false,
        };
    }

    if elapsed.get() < PIPE_HIDDEN_END_TICKS {
        return PipeTransitSample {
            position: Vec3::new(destination_center.x, hidden_y, destination_center.y),
            scale: base_scale * 0.45,
            complete: false,
        };
    }

    let t = smooth_step(
        ((elapsed_seconds - PIPE_ENTER_SECONDS - PIPE_TRAVEL_SECONDS) / PIPE_EXIT_SECONDS)
            .clamp(0.0, 1.0),
    );
    PipeTransitSample {
        position: Vec3::new(
            destination_center.x,
            hidden_y + (pipe_pair.top_y - hidden_y) * t,
            destination_center.y,
        ),
        scale: base_scale * (0.45 + (1.0 - 0.45) * t),
        complete: elapsed.get() >= PIPE_TRANSIT_END_TICKS,
    }
}

fn smooth_step(value: f32) -> f32 {
    let value = value.clamp(0.0, 1.0);
    value * value * (3.0 - 2.0 * value)
}

fn arena_hazard_marker_scale(kind: ArenaHazardKind, wave: f32) -> f32 {
    match kind {
        ArenaHazardKind::PulseVent => 1.0 + wave.max(0.0) * 0.28,
        ArenaHazardKind::SnareField => 0.94 + (wave + 1.0) * 0.08,
        ArenaHazardKind::BumperNode => 0.96 + wave.max(0.0) * 0.2,
        ArenaHazardKind::Campfire => 0.98 + (wave + 1.0) * 0.035,
        ArenaHazardKind::SawBlade => 1.0,
    }
}

pub fn advance_arena_hazards_and_collect_contacts(
    active_arena: Res<ActiveArena>,
    mut state: ResMut<ArenaHazardState>,
    match_state: Res<MatchState>,
    feel: Res<CombatFeelTuning>,
    hitstop: Res<Hitstop>,
    mut contact_buffer: ResMut<ContactBuffer>,
    fighters: Query<(
        &Fighter,
        &FighterStats,
        &FighterMotor,
        &FighterActionState,
        &SimPosition,
    )>,
) {
    if hitstop.active() {
        return;
    }

    let arena_index = active_arena.index();
    let arena = active_arena.definition();
    state.sync_to_arena(arena_index, arena.hazards.len());
    state.elapsed.advance();
    state.tick_cooldowns();

    for (hazard_index, hazard) in arena.hazards.iter().enumerate() {
        if hazard.kind == ArenaHazardKind::SawBlade && state.crank_saws_stopped {
            continue;
        }
        if !arena_hazard_is_active_for_kind_ticks(state.elapsed, hazard) {
            continue;
        }

        let Some(cooldowns) = state.hit_cooldowns.get(hazard_index) else {
            continue;
        };

        for victim in FighterId::ALL {
            let Some((fighter, stats, motor, action, transform)) = fighters
                .iter()
                .find(|(fighter, ..)| fighter.id == victim.index())
            else {
                continue;
            };
            if fighter.id >= FIGHTER_COUNT
                || !match_state.fighter_can_participate(fighter.id)
                || cooldowns[fighter.id].active()
                || !can_receive_impact(&stats, &action)
                || !arena_hazard_overlaps(hazard, transform.translation)
            {
                continue;
            }
            let (Ok(arena_index), Ok(hazard_index)) =
                (u16::try_from(arena_index), u16::try_from(hazard_index))
            else {
                continue;
            };

            let mut impact = if hazard.kind == ArenaHazardKind::BumperNode {
                bumper_impact_profile(crate::canonical_math::vec2_length(Vec2::new(
                    motor.velocity.x,
                    motor.velocity.z,
                )))
            } else {
                arena_hazard_impact_profile(hazard.kind)
            }
            .with_hit_effects_enabled(feel.hit_effects_enabled());
            if matches!(
                hazard.kind,
                ArenaHazardKind::SawBlade | ArenaHazardKind::BumperNode
            ) {
                impact.knockback_direction = Some(saw_knockback_direction(
                    transform.translation,
                    hazard.center,
                    motor.facing,
                ));
            }

            let _ = contact_buffer.push(ContactRecord::new(
                ContactPhase::Strike,
                ContactSourceKind::PersistentArenaHazard,
                ContactSourceId::ArenaHazard {
                    arena_index,
                    hazard_index,
                },
                None,
                victim,
                u16::MAX,
                crate::techniques::AttackShapeId::HazardField as u16,
                0,
                transform.translation,
                hazard.center,
                impact,
                ContactFlags::default(),
            ));
        }
    }
}

/// Commits persistent-hazard cooldowns and status accents after central impact
/// resolution. The resolver owns the semantic hit event; this consumer only
/// attaches an arena presentation sidecar to that existing event ID.
pub fn apply_arena_hazard_contact_outcomes(
    active_arena: Res<ActiveArena>,
    mut state: ResMut<ArenaHazardState>,
    contact_buffer: Res<ContactBuffer>,
    combat_intents: Option<Res<CombatPresentationIntentJournal>>,
    mut presentation_intents: Option<ResMut<ArenaPresentationIntentJournal>>,
    mut fighters: Query<(&Fighter, &mut FighterMotor, &SimPosition)>,
) {
    let arena_index = active_arena.index();
    let arena = active_arena.definition();
    for contact_index in 0..contact_buffer.len() {
        let Some(contact) = contact_buffer.record(contact_index) else {
            continue;
        };
        if contact.source_kind != ContactSourceKind::PersistentArenaHazard
            || contact.phase != ContactPhase::Strike
        {
            continue;
        }
        let Some((contact_arena, hazard_index)) = contact.source.arena_hazard() else {
            continue;
        };
        if usize::from(contact_arena) != arena_index {
            continue;
        }
        let hazard_index = usize::from(hazard_index);
        let Some(hazard) = arena.hazards.get(hazard_index) else {
            continue;
        };
        let Some(outcome) = contact_buffer.outcome(contact_index) else {
            continue;
        };
        if !matches!(
            outcome.kind,
            ContactOutcomeKind::Accepted | ContactOutcomeKind::Guarded
        ) {
            continue;
        }

        let Some((_, mut motor, transform)) = fighters
            .iter_mut()
            .find(|(fighter, ..)| fighter.id == contact.target.index())
        else {
            continue;
        };
        if hazard.kind == ArenaHazardKind::SnareField {
            motor.velocity.x *= 0.55;
            motor.velocity.z *= 0.55;
        }
        if let Some(cooldowns) = state.hit_cooldowns.get_mut(hazard_index) {
            cooldowns[contact.target.index()] =
                TickTimer::from_seconds_ceil(arena_hazard_hit_cooldown(hazard.kind));
        }

        let accent = match hazard.kind {
            ArenaHazardKind::Campfire => ArenaImpactAccent::CampfireBurn {
                duration_seconds: ARENA_HAZARD_CAMPFIRE_BURN_SECONDS,
            },
            ArenaHazardKind::SawBlade => ArenaImpactAccent::MachineScratch {
                position: transform.translation,
            },
            _ => ArenaImpactAccent::None,
        };
        if accent == ArenaImpactAccent::None {
            continue;
        }
        let (Some(event_id), Some(combat_intents), Some(presentation_intents)) = (
            outcome.event_id,
            combat_intents.as_ref(),
            presentation_intents.as_deref_mut(),
        ) else {
            continue;
        };
        let Some(combat_intent) = combat_intents.get(event_id) else {
            continue;
        };
        let _ = presentation_intents.record(ArenaPresentationIntent {
            event_id,
            victim: contact.target,
            outcome: combat_intent.outcome,
            accent,
        });
    }
}

fn saw_knockback_direction(
    fighter_position: Vec3,
    hazard_center: Vec3,
    fighter_facing: Vec3,
) -> Vec3 {
    let away_from_blade = crate::canonical_math::vec3_normalize_or_zero(Vec3::new(
        fighter_position.x - hazard_center.x,
        0.0,
        fighter_position.z - hazard_center.z,
    ));
    if crate::canonical_math::vec3_length_squared(away_from_blade) > 0.0 {
        return away_from_blade;
    }

    let away_from_arena_center = crate::canonical_math::vec3_normalize_or_zero(Vec3::new(
        hazard_center.x,
        0.0,
        hazard_center.z,
    ));
    if crate::canonical_math::vec3_length_squared(away_from_arena_center) > 0.0 {
        away_from_arena_center
    } else {
        crate::canonical_math::vec3_normalize_or(
            Vec3::new(fighter_facing.x, 0.0, fighter_facing.z),
            Vec3::X,
        )
    }
}

#[cfg(test)]
pub fn ground_height_at(x: f32, z: f32) -> Option<f32> {
    ground_height_at_with_radius(x, z, 0.0)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GroundSupport {
    Firm(f32),
    Grace(f32),
    Airborne,
}

impl GroundSupport {
    pub fn height(self) -> Option<f32> {
        match self {
            Self::Firm(height) | Self::Grace(height) => Some(height),
            Self::Airborne => None,
        }
    }
}

#[cfg(test)]
pub fn ground_height_at_with_radius(x: f32, z: f32, support_radius: f32) -> Option<f32> {
    ground_support_at_with_radius(x, z, support_radius).height()
}

#[cfg(test)]
pub fn ground_support_at_with_radius(x: f32, z: f32, support_radius: f32) -> GroundSupport {
    let arena = &arena_definitions()[0];
    ground_support_for_arena_with_radius(arena, x, z, support_radius)
}

pub fn ground_support_for_arena_with_radius(
    arena: &ArenaDefinition,
    x: f32,
    z: f32,
    support_radius: f32,
) -> GroundSupport {
    let ledge_grace =
        (support_radius * LEDGE_SUPPORT_GRACE_SCALE).clamp(0.0, LEDGE_SUPPORT_GRACE_MAX);
    let mut best = None;

    for shape in arena.ground_shapes {
        if let Some(support) = ground_shape_support(shape, x, z, ledge_grace) {
            best = Some(prefer_ground_support(best, support));
        }
    }

    for platform in arena.gameplay_platforms() {
        let dx = (x - platform.center.x).abs();
        let dz = (z - platform.center.y).abs();
        let support = if is_authored_platform(arena, platform)
            || arena.visual_theme == ArenaVisualTheme::Reactor
        {
            match platform.support_at(Vec2::new(x, z), ledge_grace) {
                Some(crate::arena_barriers::BarrierSupport::Firm) => {
                    Some(GroundSupport::Firm(platform.top_y))
                }
                Some(crate::arena_barriers::BarrierSupport::Grace) => {
                    Some(GroundSupport::Grace(platform.top_y))
                }
                None => None,
            }
        } else if let Some((outer_radius, opening_radius)) =
            circular_platform_profile(arena, platform)
        {
            debug_assert!(outer_radius >= 0.0 && opening_radius >= 0.0 && ledge_grace >= 0.0);
            let distance_squared = crate::canonical_math::vec2_length_squared(Vec2::new(
                x - platform.center.x,
                z - platform.center.y,
            ));
            if opening_radius > 0.0 && distance_squared <= opening_radius * opening_radius {
                None
            } else if distance_squared <= outer_radius * outer_radius {
                Some(GroundSupport::Firm(platform.top_y))
            } else if distance_squared
                <= (outer_radius + ledge_grace) * (outer_radius + ledge_grace)
            {
                Some(GroundSupport::Grace(platform.top_y))
            } else {
                None
            }
        } else if dx <= platform.half_extents.x && dz <= platform.half_extents.y {
            Some(GroundSupport::Firm(platform.top_y))
        } else if dx <= platform.half_extents.x + ledge_grace
            && dz <= platform.half_extents.y + ledge_grace
        {
            Some(GroundSupport::Grace(platform.top_y))
        } else {
            None
        };
        if let Some(support) = support {
            best = Some(prefer_ground_support(best, support));
        }
    }

    for collider in arena_prop_barriers(arena) {
        let support = match collider.definition.support_at(Vec2::new(x, z), ledge_grace) {
            Some(crate::arena_barriers::BarrierSupport::Firm) => {
                Some(GroundSupport::Firm(collider.definition.top_y))
            }
            Some(crate::arena_barriers::BarrierSupport::Grace) => {
                Some(GroundSupport::Grace(collider.definition.top_y))
            }
            None => None,
        };
        if let Some(support) = support {
            best = Some(prefer_ground_support(best, support));
        }
    }

    if let Some(pipe_pair) = arena.pipe_pair {
        for endpoint in pipe_pair.endpoints {
            let pipe = pipe_barrier(pipe_pair, endpoint);
            let support = match pipe.support_at(Vec2::new(x, z), ledge_grace) {
                Some(crate::arena_barriers::BarrierSupport::Firm) => {
                    Some(GroundSupport::Firm(pipe.top_y))
                }
                Some(crate::arena_barriers::BarrierSupport::Grace) => {
                    Some(GroundSupport::Grace(pipe.top_y))
                }
                None => None,
            };
            if let Some(support) = support {
                best = Some(prefer_ground_support(best, support));
            }
        }
    }

    best.unwrap_or(GroundSupport::Airborne)
}

fn ground_shape_support(
    shape: &ArenaGroundShape,
    x: f32,
    z: f32,
    ledge_grace: f32,
) -> Option<GroundSupport> {
    match *shape {
        ArenaGroundShape::Circle {
            center,
            radius,
            top_y,
        } => {
            debug_assert!(radius >= 0.0 && ledge_grace >= 0.0);
            let distance_squared =
                crate::canonical_math::vec2_length_squared(Vec2::new(x - center.x, z - center.y));
            if distance_squared <= radius * radius {
                Some(GroundSupport::Firm(top_y))
            } else if distance_squared <= (radius + ledge_grace) * (radius + ledge_grace) {
                Some(GroundSupport::Grace(top_y))
            } else {
                None
            }
        }
        ArenaGroundShape::Rectangle {
            center,
            half_extents,
            yaw,
            top_y,
        } => {
            let offset = Vec2::new(x - center.x, z - center.y);
            let (cos, sin) = crate::canonical_math::collision_yaw_basis(yaw);
            let local = Vec2::new(
                cos * offset.x + sin * offset.y,
                -sin * offset.x + cos * offset.y,
            );
            if local.x.abs() <= half_extents.x && local.y.abs() <= half_extents.y {
                Some(GroundSupport::Firm(top_y))
            } else if local.x.abs() <= half_extents.x + ledge_grace
                && local.y.abs() <= half_extents.y + ledge_grace
            {
                Some(GroundSupport::Grace(top_y))
            } else {
                None
            }
        }
    }
}

fn prefer_ground_support(
    current: Option<GroundSupport>,
    candidate: GroundSupport,
) -> GroundSupport {
    let Some(current) = current else {
        return candidate;
    };
    let current_height = current.height().unwrap_or(f32::NEG_INFINITY);
    let candidate_height = candidate.height().unwrap_or(f32::NEG_INFINITY);
    if candidate_height > current_height
        || (candidate_height == current_height
            && matches!(candidate, GroundSupport::Firm(_))
            && matches!(current, GroundSupport::Grace(_)))
    {
        candidate
    } else {
        current
    }
}

pub(crate) fn resolve_platform_side_collision_for_arena(
    arena: &ArenaDefinition,
    position: Vec3,
    radius: f32,
) -> Vec3 {
    let mut resolved = position;
    for platform in arena.gameplay_platforms() {
        resolved = if let Some((collider_radius, opening_radius)) =
            circular_platform_profile(arena, platform)
        {
            resolve_circular_platform_side_collision_against(
                resolved,
                radius,
                platform,
                collider_radius,
                opening_radius,
            )
        } else if is_authored_platform(arena, platform)
            || arena.visual_theme == ArenaVisualTheme::Reactor
        {
            if platform.top_y <= PLATFORM_SIDE_COLLISION_MIN_TOP_Y {
                resolved
            } else {
                platform.resolve_side_collision(
                    resolved,
                    radius,
                    crate::constants::LANDING_SNAP_TOLERANCE,
                )
            }
        } else {
            resolve_platform_side_collision_against(resolved, radius, platform)
        };
    }

    if let Some(pipe_pair) = arena.pipe_pair {
        for endpoint in pipe_pair.endpoints {
            let pipe = pipe_barrier(pipe_pair, endpoint);
            resolved = resolve_circular_platform_side_collision_against(
                resolved,
                radius,
                &pipe,
                pipe_pair.collider_radius,
                0.0,
            );
        }
    }
    for collider in arena_prop_barriers(arena) {
        if collider.behavior == PropBarrierBehavior::OneWayTop
            || collider.definition.top_y <= PLATFORM_SIDE_COLLISION_MIN_TOP_Y
        {
            continue;
        }
        resolved = collider.definition.resolve_side_collision(
            resolved,
            radius,
            crate::constants::LANDING_SNAP_TOLERANCE,
        );
    }
    resolved
}

fn is_authored_platform(arena: &ArenaDefinition, candidate: &PlatformDefinition) -> bool {
    arena
        .platforms
        .iter()
        .any(|platform| std::ptr::eq(platform, candidate))
}

fn circular_platform_profile(
    arena: &ArenaDefinition,
    platform: &PlatformDefinition,
) -> Option<(f32, f32)> {
    if let Some(pipe_pair) = arena.pipe_pair
        && platform.top_y == pipe_pair.top_y
        && pipe_pair.endpoints.contains(&platform.center)
    {
        // The teleport still uses trigger_radius, but the visible pipe top is a full landing disc.
        return Some((pipe_pair.collider_radius, 0.0));
    }

    None
}

fn resolve_circular_platform_side_collision_against(
    position: Vec3,
    fighter_radius: f32,
    platform: &PlatformDefinition,
    platform_radius: f32,
    opening_radius: f32,
) -> Vec3 {
    let offset = Vec2::new(
        position.x - platform.center.x,
        position.z - platform.center.y,
    );
    let expanded_radius = platform_radius + fighter_radius;
    debug_assert!(platform_radius >= 0.0 && fighter_radius >= 0.0 && opening_radius >= 0.0);
    let distance_squared = crate::canonical_math::vec2_length_squared(offset);
    let clears_lip = position.y >= platform.top_y - crate::constants::LANDING_SNAP_TOLERANCE * 2.0;

    if (opening_radius > 0.0 && distance_squared <= opening_radius * opening_radius)
        || distance_squared >= expanded_radius * expanded_radius
        || clears_lip
        || position.y > platform.top_y + 0.7
    {
        return position;
    }

    let direction = crate::canonical_math::vec2_normalize_or(offset, Vec2::X);
    Vec3::new(
        platform.center.x + direction.x * expanded_radius,
        position.y,
        platform.center.y + direction.y * expanded_radius,
    )
}

fn pipe_barrier(pipe_pair: ArenaPipePairDefinition, endpoint: Vec2) -> PlatformDefinition {
    PlatformDefinition::circle(
        endpoint.x,
        endpoint.y,
        pipe_pair.collider_radius,
        pipe_pair.top_y,
    )
}

pub fn resolve_platform_side_collision_against(
    position: Vec3,
    radius: f32,
    platform: &PlatformDefinition,
) -> Vec3 {
    if platform.top_y <= PLATFORM_SIDE_COLLISION_MIN_TOP_Y {
        return position;
    }

    let dx = position.x - platform.center.x;
    let dz = position.z - platform.center.y;
    let expanded_x = platform.half_extents.x + radius;
    let expanded_z = platform.half_extents.y + radius;
    let inside_expanded = dx.abs() < expanded_x && dz.abs() < expanded_z;
    let inside_top = dx.abs() <= platform.half_extents.x
        && dz.abs() <= platform.half_extents.y
        && position.y >= platform.top_y - 0.05;

    if !inside_expanded || inside_top || position.y > platform.top_y + 0.7 {
        return position;
    }

    let push_x = expanded_x - dx.abs();
    let push_z = expanded_z - dz.abs();
    if push_x < push_z {
        Vec3::new(
            platform.center.x + expanded_x * dx.signum(),
            position.y,
            position.z,
        )
    } else {
        Vec3::new(
            position.x,
            position.y,
            platform.center.y + expanded_z * dz.signum(),
        )
    }
}

#[cfg(test)]
pub fn arena_hazard_is_active(elapsed: f32, pulse_seconds: f32) -> bool {
    let cycle = pulse_seconds.max(0.1);
    elapsed.rem_euclid(cycle) <= cycle * 0.36
}

#[cfg(test)]
pub fn arena_hazard_is_active_for_kind(elapsed: f32, hazard: &ArenaHazardDefinition) -> bool {
    let cycle = hazard.pulse_seconds.max(0.1);
    (elapsed + hazard.phase).rem_euclid(cycle) <= cycle * arena_hazard_active_fraction(hazard.kind)
}

pub fn arena_hazard_is_active_for_kind_ticks(
    elapsed: ElapsedTicks,
    hazard: &ArenaHazardDefinition,
) -> bool {
    let cycle_ticks = seconds_to_ticks_ceil(hazard.pulse_seconds.max(0.1)).max(1);
    let phase_ticks = seconds_to_ticks_ceil(hazard.phase);
    let cycle_position =
        ((u64::from(elapsed.get()) + u64::from(phase_ticks)) % u64::from(cycle_ticks)) as u32;
    let active_ticks = seconds_to_ticks_ceil(
        hazard.pulse_seconds.max(0.1) * arena_hazard_active_fraction(hazard.kind),
    )
    .clamp(1, cycle_ticks);
    cycle_position < active_ticks
}

fn arena_hazard_active_fraction(kind: ArenaHazardKind) -> f32 {
    match kind {
        ArenaHazardKind::PulseVent => 0.32,
        ArenaHazardKind::SnareField => 0.68,
        ArenaHazardKind::BumperNode => 1.0,
        ArenaHazardKind::Campfire => 1.0,
        ArenaHazardKind::SawBlade => 1.0,
    }
}

fn arena_hazard_hit_cooldown(kind: ArenaHazardKind) -> f32 {
    match kind {
        ArenaHazardKind::PulseVent => 1.05,
        ArenaHazardKind::SnareField => 0.56,
        ArenaHazardKind::BumperNode => 0.82,
        ArenaHazardKind::Campfire => 0.82,
        ArenaHazardKind::SawBlade => 0.68,
    }
}

fn arena_hazard_overlaps(hazard: &ArenaHazardDefinition, fighter_position: Vec3) -> bool {
    let flat = Vec2::new(
        fighter_position.x - hazard.center.x,
        fighter_position.z - hazard.center.z,
    );
    let expanded_radius = hazard.radius + FIGHTER_RADIUS;
    debug_assert!(hazard.radius >= 0.0 && expanded_radius >= 0.0);
    crate::canonical_math::vec2_length_squared(flat) <= expanded_radius * expanded_radius
        && arena_hazard_affects_height(hazard, fighter_position.y)
}

pub fn arena_hazard_affects_height(hazard: &ArenaHazardDefinition, fighter_y: f32) -> bool {
    let offset = fighter_y - hazard.center.y;
    let (below, above) = match hazard.kind {
        ArenaHazardKind::PulseVent => (0.32, 2.35),
        ArenaHazardKind::SnareField => (0.35, 0.8),
        ArenaHazardKind::BumperNode => (0.45, 1.45),
        ArenaHazardKind::Campfire => (0.3, 1.55),
        ArenaHazardKind::SawBlade => (0.4, 1.35),
    };
    offset >= -below && offset <= above
}

fn arena_hazard_impact_profile(kind: ArenaHazardKind) -> ImpactProfile {
    let mut profile = match kind {
        ArenaHazardKind::PulseVent => impact_profile(
            NEUTRAL_IMPACT_OWNER_ID,
            ImpactSource::Hazard,
            ARENA_HAZARD_PULSE_DAMAGE,
            ARENA_HAZARD_PULSE_KNOCKBACK,
            4.1,
            true,
            true,
            16.0,
            ImpactFeedbackIntensity::Heavy,
            ReactionFamilyId::LauncherDown,
        ),
        ArenaHazardKind::SnareField => impact_profile(
            NEUTRAL_IMPACT_OWNER_ID,
            ImpactSource::Hazard,
            ARENA_HAZARD_SNARE_DAMAGE,
            ARENA_HAZARD_SNARE_KNOCKBACK,
            1.0,
            false,
            true,
            10.0,
            ImpactFeedbackIntensity::Light,
            ReactionFamilyId::ShortStandingStagger,
        ),
        ArenaHazardKind::BumperNode => bumper_impact_profile(0.0),
        ArenaHazardKind::Campfire => impact_profile(
            NEUTRAL_IMPACT_OWNER_ID,
            ImpactSource::Hazard,
            ARENA_HAZARD_CAMPFIRE_DAMAGE,
            ARENA_HAZARD_CAMPFIRE_KNOCKBACK,
            ARENA_HAZARD_CAMPFIRE_LAUNCH,
            true,
            false,
            12.0,
            ImpactFeedbackIntensity::Heavy,
            ReactionFamilyId::LauncherDown,
        ),
        ArenaHazardKind::SawBlade => impact_profile(
            NEUTRAL_IMPACT_OWNER_ID,
            ImpactSource::Hazard,
            ARENA_HAZARD_SAW_DAMAGE,
            ARENA_HAZARD_SAW_KNOCKBACK,
            ARENA_HAZARD_SAW_LAUNCH,
            true,
            false,
            18.0,
            ImpactFeedbackIntensity::Heavy,
            ReactionFamilyId::LauncherDown,
        ),
    };
    profile.element = DamageElement::Hazard;
    profile
}

fn bumper_impact_profile(planar_speed: f32) -> ImpactProfile {
    let speed_factor = ((planar_speed - 2.0) / 9.0).clamp(0.0, 1.0);
    impact_profile(
        NEUTRAL_IMPACT_OWNER_ID,
        ImpactSource::Hazard,
        ARENA_HAZARD_BUMPER_DAMAGE * (0.45 + speed_factor * 1.55),
        ARENA_HAZARD_BUMPER_KNOCKBACK * (0.8 + speed_factor * 1.0),
        2.4 + speed_factor * 4.2,
        speed_factor >= 0.62,
        true,
        16.0 + speed_factor * 12.0,
        ImpactFeedbackIntensity::Heavy,
        if speed_factor >= 0.62 {
            ReactionFamilyId::LauncherDown
        } else {
            ReactionFamilyId::LightAirPop
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arena_barriers::BarrierFootprint;
    use crate::characters::{CharacterKind, CharacterMoveCatalog, FighterCharacter};
    use crate::combat::{
        CombatPresentationIntentJournal, DamageDefenderProfile, HitEffects, apply_impact_core,
        begin_contact_collection, impact_sim_event_kind, present_committed_combat_events,
        resolve_contacts,
    };
    use crate::components::{FighterGrabState, FighterUltimateState};
    use crate::ecs_identity::SimulationIdentityAllocator;
    use crate::equipment::FighterEquipment;
    use crate::game_state::MatchTelemetry;
    use crate::sim_event::{
        PresentationEventCursor, PresentationEventRouter, SimEventJournal, SimEventSource,
        TickEventBuffer,
    };
    use crate::styles::FighterStyle;

    fn arena_presentation_test_outcome() -> ImpactOutcome {
        let state = MatchState::default();
        let mut stats = FighterStats::default();
        let mut motor = FighterMotor {
            grounded: true,
            facing: Vec3::Z,
            ..default()
        };
        let mut action = FighterActionState::default();
        let mut hitstop = Hitstop::default();
        let mut telemetry = MatchTelemetry::default();
        apply_impact_core(
            &mut hitstop,
            &state,
            &mut stats,
            &mut motor,
            &mut action,
            Vec3::ZERO,
            None,
            Vec3::NEG_Z,
            arena_hazard_impact_profile(ArenaHazardKind::Campfire),
            DamageDefenderProfile::default(),
            &mut telemetry,
        )
    }

    fn hazard_test_app(kind: ArenaHazardKind, with_presentation: bool) -> (App, Entity) {
        let arena_index = arena_definitions()
            .iter()
            .position(|arena| arena.hazards.iter().any(|hazard| hazard.kind == kind))
            .expect("fixture arena must contain requested hazard");
        let active = ActiveArena::new(arena_index);
        let hazard = active
            .definition()
            .hazards
            .iter()
            .find(|hazard| hazard.kind == kind)
            .copied()
            .unwrap();
        let mut state = MatchState::default();
        state.set_active_slots([true, false, false, false]);
        state.reset_for_new_match();

        let mut app = App::new();
        app.insert_resource(active)
            .insert_resource(SimulationIdentityAllocator::default())
            .insert_resource(ArenaHazardState::new(
                arena_index,
                active.definition().hazards.len(),
            ))
            .insert_resource(state)
            .insert_resource(CombatFeelTuning::default())
            .insert_resource(CharacterMoveCatalog::default())
            .insert_resource(Hitstop::default())
            .insert_resource(MatchTelemetry::default())
            .insert_resource(ContactBuffer::default())
            .insert_resource(TickEventBuffer::new(SimTick(7)))
            .add_systems(
                Update,
                (
                    begin_contact_collection,
                    advance_arena_hazards_and_collect_contacts,
                    resolve_contacts,
                    apply_arena_hazard_contact_outcomes,
                )
                    .chain(),
            );
        if with_presentation {
            app.insert_resource(ArenaPresentationIntentJournal::default())
                .insert_resource(CombatPresentationIntentJournal::default());
        }
        let fighter = app
            .world_mut()
            .spawn((
                Fighter {
                    id: 0,
                    name: "Arena target",
                    color: Color::WHITE,
                    spawn: hazard.center,
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
                    kind: crate::styles::FighterStyleKind::Anchor,
                },
                FighterEquipment::new(crate::equipment::EquipmentKind::CounterCell),
                SimPosition::new(hazard.center),
            ))
            .id();
        (app, fighter)
    }

    fn arena_presentation_test_app() -> App {
        let mut app = App::new();
        app.insert_resource(EffectAssets::presentation_enabled_for_test())
            .insert_resource(HitEffects::default())
            .insert_resource(SimEventJournal::default())
            .insert_resource(CombatPresentationIntentJournal::default())
            .insert_resource(ArenaPresentationIntentJournal::default())
            .insert_resource(PresentationEventCursor::default())
            .insert_resource(PresentationEventRouter::default())
            .add_systems(Update, present_committed_combat_events);
        app.world_mut().spawn((
            Fighter {
                id: 0,
                name: "Arena presentation target",
                color: Color::WHITE,
                spawn: Vec3::ZERO,
            },
            FighterStats::default(),
            FighterMotor::default(),
            FighterActionState::default(),
        ));
        app
    }

    fn commit_arena_presentation(
        app: &mut App,
        tick: u64,
        accent: ArenaImpactAccent,
    ) -> SimEventId {
        let outcome = arena_presentation_test_outcome();
        let mut buffer = TickEventBuffer::new(SimTick(tick));
        let event_id = buffer
            .emit(
                SimEventSource::Arena,
                impact_sim_event_kind(outcome, None, FighterId::ZERO),
            )
            .unwrap();
        app.world_mut()
            .resource_mut::<SimEventJournal>()
            .commit(&buffer);
        app.world_mut()
            .resource_mut::<ArenaPresentationIntentJournal>()
            .record(ArenaPresentationIntent {
                event_id,
                victim: FighterId::ZERO,
                outcome,
                accent,
            })
            .unwrap();
        event_id
    }

    fn arena_effect_count(app: &mut App, kind: crate::effects::EffectKind) -> usize {
        let world = app.world_mut();
        let mut effects = world.query::<&crate::effects::VisualEffect>();
        effects
            .iter(world)
            .filter(|effect| effect.kind == kind)
            .count()
    }

    #[test]
    fn headless_hazard_hit_commits_state_and_event_without_presentation_resources() {
        let (mut app, fighter) = hazard_test_app(ArenaHazardKind::Campfire, false);

        app.update();

        let stats = app.world().get::<FighterStats>(fighter).unwrap();
        assert!(stats.health < crate::constants::MAX_HEALTH);
        assert_eq!(stats.hud_flash, 0.0);
        assert!(
            app.world().get::<ArenaFighterBurn>(fighter).is_none(),
            "burn tint is render-local and must not enter the headless world"
        );
        assert!(app.world().get_resource::<EffectAssets>().is_none());
        assert!(
            app.world()
                .get_resource::<ArenaPresentationIntentJournal>()
                .is_none()
        );
        let events = app.world().resource::<TickEventBuffer>();
        assert_eq!(events.len(), 1);
        assert!(matches!(
            events.iter().next().unwrap().kind,
            crate::sim_event::SimEventKind::HitConfirmed {
                attacker: None,
                victim: FighterId::ZERO,
                ..
            }
        ));
        let health_after_first = stats.health;
        assert!(
            app.world()
                .resource::<ArenaHazardState>()
                .hit_cooldowns
                .iter()
                .any(|cooldowns| cooldowns[FighterId::ZERO.index()].active()),
            "an accepted persistent-hazard contact starts its per-target cooldown"
        );
        app.update();
        assert_eq!(
            app.world().get::<FighterStats>(fighter).unwrap().health,
            health_after_first,
            "an active per-target cooldown suppresses the next overlapping tick"
        );
        assert_eq!(app.world().resource::<TickEventBuffer>().len(), 1);
        let world = app.world_mut();
        let mut effects = world.query::<&crate::effects::VisualEffect>();
        assert_eq!(effects.iter(world).count(), 0);
    }

    #[test]
    fn hazard_hit_records_sidecar_without_presenting_during_simulation() {
        let (mut app, fighter) = hazard_test_app(ArenaHazardKind::SawBlade, true);

        app.update();

        assert!(
            app.world().get::<FighterStats>(fighter).unwrap().health < crate::constants::MAX_HEALTH
        );
        assert_eq!(
            app.world()
                .resource::<ArenaPresentationIntentJournal>()
                .len(),
            1
        );
        assert_eq!(
            app.world().get::<FighterStats>(fighter).unwrap().hud_flash,
            0.0
        );
        let world = app.world_mut();
        let mut effects = world.query::<&crate::effects::VisualEffect>();
        assert_eq!(effects.iter(world).count(), 0);
    }

    #[derive(Debug, PartialEq, Eq)]
    struct HazardStrikeFixture {
        health_q: i32,
        outcomes: Vec<ContactOutcomeKind>,
        event_sources: Vec<SimEventSource>,
        cooldown_active: bool,
    }

    fn run_hazard_and_strike_fixture(reverse_insert_order: bool) -> HazardStrikeFixture {
        let arena_index = arena_definitions()
            .iter()
            .position(|arena| {
                arena
                    .hazards
                    .iter()
                    .any(|hazard| hazard.kind == ArenaHazardKind::Campfire)
            })
            .unwrap();
        let active_arena = ActiveArena::new(arena_index);
        let (hazard_index, hazard) = active_arena
            .definition()
            .hazards
            .iter()
            .copied()
            .enumerate()
            .find(|(_, hazard)| hazard.kind == ArenaHazardKind::Campfire)
            .unwrap();
        let owner = FighterId::ZERO;
        let target = FighterId::new(1).unwrap();

        let mut match_state = MatchState::default();
        match_state.set_active_slots([true, true, false, false]);
        match_state.reset_for_new_match();
        let mut app = App::new();
        app.insert_resource(active_arena)
            .insert_resource(ArenaHazardState::new(
                arena_index,
                active_arena.definition().hazards.len(),
            ))
            .insert_resource(match_state)
            .insert_resource(CharacterMoveCatalog::default())
            .insert_resource(Hitstop::default())
            .insert_resource(MatchTelemetry::default())
            .insert_resource(TickEventBuffer::new(SimTick(41)));

        let spawn_fighter = |world: &mut World, fighter_id: FighterId, position: Vec3| {
            world.spawn((
                Fighter {
                    id: fighter_id.index(),
                    name: "Hazard and strike",
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
                    kind: crate::styles::FighterStyleKind::Anchor,
                },
                FighterEquipment::new(crate::equipment::EquipmentKind::CounterCell),
                SimPosition::new(position),
            ));
        };
        if reverse_insert_order {
            spawn_fighter(app.world_mut(), target, hazard.center);
            spawn_fighter(app.world_mut(), owner, hazard.center + Vec3::X * 3.0);
        } else {
            spawn_fighter(app.world_mut(), owner, hazard.center + Vec3::X * 3.0);
            spawn_fighter(app.world_mut(), target, hazard.center);
        }

        let source_entity = app.world_mut().spawn_empty().id();
        let mut identities = SimulationIdentityAllocator::default();
        let stable_source = identities
            .try_allocate(SimEntityKind::Hitbox, source_entity)
            .unwrap();
        app.world_mut()
            .entity_mut(source_entity)
            .insert(stable_source);
        app.insert_resource(identities);

        let strike = ContactRecord::new(
            ContactPhase::Strike,
            ContactSourceKind::FighterStrike,
            stable_source.id(),
            Some(owner),
            target,
            u16::MAX,
            crate::techniques::AttackShapeId::CompactSlashLead as u16,
            0,
            hazard.center,
            hazard.center + Vec3::X * 0.5,
            impact_profile(
                owner.index(),
                ImpactSource::FighterStrike,
                5.0,
                3.0,
                1.0,
                false,
                true,
                8.0,
                ImpactFeedbackIntensity::Light,
                ReactionFamilyId::ShortStandingStagger,
            ),
            ContactFlags::default(),
        );
        let hazard_contact = ContactRecord::new(
            ContactPhase::Strike,
            ContactSourceKind::PersistentArenaHazard,
            ContactSourceId::ArenaHazard {
                arena_index: u16::try_from(arena_index).unwrap(),
                hazard_index: u16::try_from(hazard_index).unwrap(),
            },
            None,
            target,
            u16::MAX,
            crate::techniques::AttackShapeId::HazardField as u16,
            0,
            hazard.center,
            hazard.center,
            arena_hazard_impact_profile(hazard.kind),
            ContactFlags::default(),
        );
        let mut contacts = ContactBuffer::default();
        if reverse_insert_order {
            contacts.push(hazard_contact);
            contacts.push(strike);
        } else {
            contacts.push(strike);
            contacts.push(hazard_contact);
        }
        app.insert_resource(contacts).add_systems(
            Update,
            (resolve_contacts, apply_arena_hazard_contact_outcomes).chain(),
        );

        app.update();

        let health_q = {
            let world = app.world_mut();
            let mut fighters = world.query::<(&Fighter, &FighterStats)>();
            let (_, stats) = fighters
                .iter(world)
                .find(|(fighter, _)| fighter.id == target.index())
                .unwrap();
            quantize_f32(stats.health, DEFAULT_F32_QUANTIZATION)
        };
        let outcomes = {
            let contacts = app.world().resource::<ContactBuffer>();
            (0..contacts.len())
                .map(|index| contacts.outcome(index).unwrap().kind)
                .collect()
        };
        let event_sources = app
            .world()
            .resource::<TickEventBuffer>()
            .iter()
            .map(|event| event.id.source)
            .collect();
        let cooldown_active = app.world().resource::<ArenaHazardState>().hit_cooldowns
            [hazard_index][target.index()]
        .active();
        HazardStrikeFixture {
            health_q,
            outcomes,
            event_sources,
            cooldown_active,
        }
    }

    #[test]
    fn hazard_and_strike_both_land_independent_of_insertion_and_ecs_order() {
        let forward = run_hazard_and_strike_fixture(false);
        let reversed = run_hazard_and_strike_fixture(true);

        assert_eq!(forward, reversed);
        assert_eq!(
            forward.outcomes,
            vec![ContactOutcomeKind::Accepted, ContactOutcomeKind::Accepted]
        );
        assert_eq!(forward.event_sources.len(), 2);
        assert!(forward.cooldown_active);
        assert!(
            forward.health_q < quantize_f32(crate::constants::MAX_HEALTH, DEFAULT_F32_QUANTIZATION)
        );
    }

    #[test]
    fn arena_presentation_intent_storage_is_bounded_and_rollback_discardable() {
        let outcome = arena_presentation_test_outcome();
        let tick = SimTick(80);
        let mut intents = ArenaPresentationIntentJournal::default();
        for ordinal in 0..MAX_SIM_EVENTS_PER_TICK {
            intents
                .record(ArenaPresentationIntent {
                    event_id: SimEventId {
                        tick,
                        source: SimEventSource::Arena,
                        ordinal: ordinal as u16,
                    },
                    victim: FighterId::ZERO,
                    outcome,
                    accent: ArenaImpactAccent::None,
                })
                .unwrap();
        }
        assert_eq!(intents.len(), MAX_SIM_EVENTS_PER_TICK);
        assert_eq!(
            intents.capacity(),
            SIM_EVENT_HISTORY_TICKS * MAX_SIM_EVENTS_PER_TICK
        );
        assert_eq!(
            intents.record(ArenaPresentationIntent {
                event_id: SimEventId {
                    tick,
                    source: SimEventSource::Arena,
                    ordinal: MAX_SIM_EVENTS_PER_TICK as u16,
                },
                victim: FighterId::ZERO,
                outcome,
                accent: ArenaImpactAccent::None,
            }),
            Err(EventEmitError::CapacityExceeded {
                capacity: MAX_SIM_EVENTS_PER_TICK,
            })
        );

        intents.discard_after(SimTick(79));
        assert_eq!(intents.len(), 0);
        assert_eq!(intents.metrics().discarded, MAX_SIM_EVENTS_PER_TICK as u64);
        assert_eq!(intents.metrics().rejected, 1);
    }

    #[test]
    fn arena_events_survive_render_stall_and_rollback_replay_exactly_once() {
        let mut app = arena_presentation_test_app();
        let burn = commit_arena_presentation(
            &mut app,
            90,
            ArenaImpactAccent::CampfireBurn {
                duration_seconds: ARENA_HAZARD_CAMPFIRE_BURN_SECONDS,
            },
        );
        let scratch = commit_arena_presentation(
            &mut app,
            91,
            ArenaImpactAccent::MachineScratch {
                position: Vec3::ZERO,
            },
        );

        // One render update observes both fixed ticks after the simulated stall.
        app.update();
        assert_eq!(
            arena_effect_count(&mut app, crate::effects::EffectKind::Burning),
            7
        );
        let world = app.world_mut();
        let mut fighters = world.query_filtered::<&ArenaFighterBurn, With<Fighter>>();
        assert_eq!(fighters.iter(world).count(), 1);
        assert_eq!(
            arena_effect_count(&mut app, crate::effects::EffectKind::Scratch),
            12
        );
        let first_total = {
            let world = app.world_mut();
            let mut effects = world.query::<&crate::effects::VisualEffect>();
            effects.iter(world).count()
        };
        assert_eq!(
            app.world_mut()
                .resource_mut::<HitEffects>()
                .drain_combat_sfx_cues()
                .len(),
            2
        );
        assert_eq!(
            app.world()
                .resource::<PresentationEventCursor>()
                .metrics()
                .observed_ticks,
            2
        );

        let retained = SimTick(89);
        app.world_mut()
            .resource_mut::<PresentationEventCursor>()
            .discard_after(retained);
        app.world_mut()
            .resource_mut::<PresentationEventRouter>()
            .discard_after(retained);
        app.world_mut()
            .resource_mut::<SimEventJournal>()
            .discard_after(retained);
        app.world_mut()
            .resource_mut::<ArenaPresentationIntentJournal>()
            .discard_after(retained);
        assert_eq!(
            commit_arena_presentation(
                &mut app,
                90,
                ArenaImpactAccent::CampfireBurn {
                    duration_seconds: ARENA_HAZARD_CAMPFIRE_BURN_SECONDS,
                },
            ),
            burn
        );
        assert_eq!(
            commit_arena_presentation(
                &mut app,
                91,
                ArenaImpactAccent::MachineScratch {
                    position: Vec3::ZERO,
                },
            ),
            scratch
        );

        app.update();

        assert_eq!(
            arena_effect_count(&mut app, crate::effects::EffectKind::Burning),
            7
        );
        assert_eq!(
            arena_effect_count(&mut app, crate::effects::EffectKind::Scratch),
            12
        );
        let replay_total = {
            let world = app.world_mut();
            let mut effects = world.query::<&crate::effects::VisualEffect>();
            effects.iter(world).count()
        };
        assert_eq!(replay_total, first_total);
        assert!(
            app.world_mut()
                .resource_mut::<HitEffects>()
                .drain_combat_sfx_cues()
                .is_empty()
        );
        assert_eq!(
            app.world()
                .resource::<PresentationEventRouter>()
                .metrics()
                .duplicate_events_suppressed,
            2
        );
    }

    #[test]
    fn canonical_arena_bootstrap_is_headless_idempotent_and_switch_safe() {
        let mut world = World::new();
        let selected = bootstrap_canonical_arena_runtime(&mut world, 0);
        assert_eq!(selected.index(), 0);
        assert_eq!(world.resource::<ActiveArena>().index(), 0);
        assert_eq!(
            world.resource::<ArenaHazardState>().hit_cooldowns.len(),
            selected.definition().hazards.len()
        );
        assert_eq!(world.resource::<ArenaPipeState>().arena_index, 0);
        assert_eq!(world.resource::<PowderKegCannonState>().arena_index, 0);

        assert!(world.get_resource::<ArenaScene>().is_none());
        assert!(world.get_resource::<ArenaOrdnanceAssets>().is_none());
        assert!(world.get_resource::<AssetServer>().is_none());
        assert!(world.get_resource::<Assets<Mesh>>().is_none());
        assert!(world.get_resource::<Assets<StandardMaterial>>().is_none());
        let geometry_count = world
            .query_filtered::<Entity, With<ArenaGeometry>>()
            .iter(&world)
            .count();
        assert_eq!(geometry_count, 0);

        world.resource_mut::<ArenaHazardState>().elapsed = ElapsedTicks::from_ticks(73);
        world.resource_mut::<PowderKegCannonState>().fire_timer = TickTimer::from_ticks(11);
        let before_repeat = capture_arena_runtime_snapshot(&world).unwrap();
        bootstrap_canonical_arena_runtime(&mut world, 0);
        assert_eq!(
            capture_arena_runtime_snapshot(&world).unwrap(),
            before_repeat,
            "same-arena bootstrap must not rewind live authoritative state"
        );

        let switched = bootstrap_canonical_arena_runtime(&mut world, CRANK_YARD_ARENA_INDEX);
        let switched_snapshot = capture_arena_runtime_snapshot(&world).unwrap();
        let mut fresh = World::new();
        bootstrap_canonical_arena_runtime(&mut fresh, CRANK_YARD_ARENA_INDEX);
        assert_eq!(switched.index(), CRANK_YARD_ARENA_INDEX);
        assert_eq!(
            switched_snapshot,
            capture_arena_runtime_snapshot(&fresh).unwrap(),
            "an arena switch must reset to the same canonical state as a fresh world"
        );
    }

    #[test]
    fn arena_render_generation_is_monotonic_across_repeated_indices() {
        let mut scene = ArenaScene::new(0);
        assert_eq!(scene.index(), 0);
        assert_eq!(scene.generation(), 1);

        assert_eq!(scene.rebuild(CRANK_YARD_ARENA_INDEX), 2);
        assert_eq!(scene.index(), CRANK_YARD_ARENA_INDEX);
        assert_eq!(scene.rebuild(0), 3);
        assert_eq!(scene.index(), 0);
        assert_eq!(scene.generation(), 3);

        let stale = ArenaSceneReadyMarker {
            arena_index: 0,
            generation: 1,
        };
        assert_eq!(stale.arena_index(), scene.index());
        assert_ne!(stale.generation(), scene.generation());
    }

    #[test]
    #[should_panic(expected = "arena render generation exhausted")]
    fn arena_render_generation_fails_closed_on_overflow() {
        let mut scene = ArenaScene {
            index: 0,
            generation: u64::MAX,
        };
        scene.rebuild(1);
    }

    fn powder_keg_test_app(active_arena: usize) -> App {
        let mut app = App::new();
        app.insert_resource(ActiveArena::new(active_arena))
            .insert_resource(SimulationIdentityAllocator::default())
            .insert_resource(PowderKegCannonState::new(active_arena))
            .insert_resource(ArenaOrdnanceContactFrame::default())
            .insert_resource(MatchState::default())
            .insert_resource(CombatFeelTuning::default())
            .insert_resource(CharacterMoveCatalog::default())
            .insert_resource(Hitstop::default())
            .insert_resource(MatchTelemetry::default())
            .insert_resource(ContactBuffer::default())
            .insert_resource(SimTick::default())
            .insert_resource(TickEventBuffer::default())
            .add_systems(
                Update,
                (
                    begin_contact_collection,
                    advance_powder_keg_cannons_and_collect_contacts,
                    resolve_contacts,
                    apply_powder_keg_contact_outcomes,
                )
                    .chain(),
            );
        app
    }

    #[test]
    fn leaving_powder_keg_releases_authoritative_ordnance() {
        let mut app = powder_keg_test_app(0);
        let bomb_entity = app
            .world_mut()
            .spawn((
                ArenaCannonBomb {
                    velocity: Vec3::ZERO,
                    lifetime: TickTimer::from_ticks(60),
                },
                SimPosition::default(),
            ))
            .id();
        let stable = app
            .world_mut()
            .resource_mut::<SimulationIdentityAllocator>()
            .try_allocate(SimEntityKind::ArenaOrdnance, bomb_entity)
            .expect("test ordnance pool has room");
        app.world_mut().entity_mut(bomb_entity).insert(stable);

        app.update();

        assert!(app.world().get_entity(bomb_entity).is_err());
        assert_eq!(
            app.world()
                .resource::<SimulationIdentityAllocator>()
                .live_count(SimEntityKind::ArenaOrdnance),
            0
        );
    }

    #[test]
    fn powder_keg_bombs_are_not_tagged_as_render_owned_arena_geometry() {
        let mut app = powder_keg_test_app(POWDER_KEG_ARENA_INDEX);
        app.world_mut()
            .resource_mut::<PowderKegCannonState>()
            .fire_timer = TickTimer::from_ticks(1);

        app.update();

        let bomb_entity = app
            .world_mut()
            .query_filtered::<Entity, With<ArenaCannonBomb>>()
            .single(app.world())
            .expect("the cannon should spawn one bomb");
        assert!(app.world().get::<StableSimEntity>(bomb_entity).is_some());
        assert!(app.world().get::<ArenaGeometry>(bomb_entity).is_none());
        assert!(app.world().get::<Mesh3d>(bomb_entity).is_none());
    }

    #[test]
    fn headless_cannon_hit_emits_neutral_impact_without_inline_feedback() {
        let mut app = powder_keg_test_app(POWDER_KEG_ARENA_INDEX);
        let position = Vec3::new(0.0, ARENA_TOP_Y + 0.7, 0.0);
        let fighter = app
            .world_mut()
            .spawn((
                Fighter {
                    id: 0,
                    name: "Cannon target",
                    color: Color::WHITE,
                    spawn: position,
                },
                FighterCharacter::new(CharacterKind::Cat),
                FighterStats::default(),
                FighterMotor {
                    grounded: true,
                    ..default()
                },
                FighterActionState::default(),
                FighterGrabState::default(),
                FighterUltimateState::default(),
                FighterStyle {
                    kind: crate::styles::FighterStyleKind::Anchor,
                },
                FighterEquipment::new(crate::equipment::EquipmentKind::CounterCell),
                SimPosition::new(position),
            ))
            .id();
        let bomb = app
            .world_mut()
            .spawn((
                ArenaCannonBomb {
                    velocity: Vec3::ZERO,
                    lifetime: TickTimer::from_ticks(60),
                },
                SimPosition::new(position + Vec3::Y * 0.2),
            ))
            .id();
        let stable = app
            .world_mut()
            .resource_mut::<SimulationIdentityAllocator>()
            .try_allocate(SimEntityKind::ArenaOrdnance, bomb)
            .unwrap();
        app.world_mut().entity_mut(bomb).insert(stable);

        app.update();

        assert!(app.world().get_entity(bomb).is_err());
        let stats = app.world().get::<FighterStats>(fighter).unwrap();
        assert!(stats.health < crate::constants::MAX_HEALTH);
        assert_eq!(stats.hud_flash, 0.0);
        assert!(app.world().get_resource::<EffectAssets>().is_none());
        let events = app.world().resource::<TickEventBuffer>();
        assert_eq!(events.len(), 1);
        assert!(matches!(
            events.iter().next().unwrap().kind,
            crate::sim_event::SimEventKind::HitConfirmed {
                attacker: None,
                victim: FighterId::ZERO,
                ..
            }
        ));
    }

    #[derive(Debug, PartialEq)]
    struct CannonMultiTargetFixture {
        health: [f32; 2],
        victims: Vec<FighterId>,
        source: SimEntityId,
        live_ordnance: u32,
    }

    fn run_cannon_multi_target_fixture(reverse_ecs_allocation: bool) -> CannonMultiTargetFixture {
        let mut app = powder_keg_test_app(POWDER_KEG_ARENA_INDEX);
        let mut match_state = MatchState::default();
        match_state.set_active_slots([true, true, false, false]);
        match_state.reset_for_new_match();
        app.insert_resource(match_state);

        if reverse_ecs_allocation {
            app.world_mut().spawn_empty();
        }
        let fighter_position = |fighter: FighterId| {
            Vec3::new(
                if fighter == FighterId::ZERO {
                    -0.2
                } else {
                    0.2
                },
                ARENA_TOP_Y,
                0.0,
            )
        };
        let spawn_fighter = |world: &mut World, fighter_id: FighterId| {
            let position = fighter_position(fighter_id);
            world.spawn((
                Fighter {
                    id: fighter_id.index(),
                    name: "Cannon multi-target",
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
                    kind: crate::styles::FighterStyleKind::Anchor,
                },
                FighterEquipment::new(crate::equipment::EquipmentKind::CounterCell),
                SimPosition::new(position),
            ));
        };
        let fighter_one = FighterId::new(1).unwrap();
        let fighter_order = if reverse_ecs_allocation {
            [fighter_one, FighterId::ZERO]
        } else {
            [FighterId::ZERO, fighter_one]
        };
        for fighter_id in fighter_order {
            spawn_fighter(app.world_mut(), fighter_id);
        }

        let bomb_position = Vec3::new(0.0, ARENA_TOP_Y + 0.72, 0.0);
        let bomb = app
            .world_mut()
            .spawn((
                ArenaCannonBomb {
                    velocity: Vec3::ZERO,
                    lifetime: TickTimer::from_ticks(60),
                },
                SimPosition::new(bomb_position),
            ))
            .id();
        let stable = app
            .world_mut()
            .resource_mut::<SimulationIdentityAllocator>()
            .try_allocate(SimEntityKind::ArenaOrdnance, bomb)
            .unwrap();
        app.world_mut().entity_mut(bomb).insert(stable);

        app.update();

        let mut health = [0.0; 2];
        {
            let world = app.world_mut();
            let mut fighters = world.query::<(&Fighter, &FighterStats)>();
            for (fighter, stats) in fighters.iter(world) {
                if fighter.id < health.len() {
                    health[fighter.id] = stats.health;
                }
            }
        }
        let victims = app
            .world()
            .resource::<TickEventBuffer>()
            .iter()
            .filter_map(|event| match event.kind {
                crate::sim_event::SimEventKind::HitConfirmed { victim, .. } => Some(victim),
                _ => None,
            })
            .collect();
        CannonMultiTargetFixture {
            health,
            victims,
            source: stable.id(),
            live_ordnance: app
                .world()
                .resource::<SimulationIdentityAllocator>()
                .live_count(SimEntityKind::ArenaOrdnance),
        }
    }

    #[test]
    fn cannon_projectile_freezes_all_targets_and_ignores_ecs_allocation_order() {
        let forward = run_cannon_multi_target_fixture(false);
        let reversed = run_cannon_multi_target_fixture(true);

        assert_eq!(forward, reversed);
        assert!(
            forward
                .health
                .iter()
                .all(|health| *health < crate::constants::MAX_HEALTH)
        );
        assert_eq!(
            forward.victims,
            vec![FighterId::ZERO, FighterId::new(1).unwrap()]
        );
        assert_eq!(forward.live_ordnance, 0);
        assert_eq!(forward.source.kind(), SimEntityKind::ArenaOrdnance);
    }

    #[test]
    fn cannon_bomb_visual_is_reconciled_as_a_render_only_child() {
        let mut app = App::new();
        app.insert_resource(Time::<()>::default())
            .insert_resource(ArenaOrdnanceAssets::default())
            .add_systems(Update, sync_arena_cannon_bomb_visuals);
        let bomb = app
            .world_mut()
            .spawn((
                ArenaCannonBomb {
                    velocity: Vec3::X,
                    lifetime: TickTimer::from_ticks(5),
                },
                SimPosition::new(Vec3::new(1.0, 2.0, 3.0)),
            ))
            .id();

        app.update();

        assert!(app.world().get::<Mesh3d>(bomb).is_none());
        assert_eq!(
            app.world().get::<Transform>(bomb).unwrap().translation,
            Vec3::new(1.0, 2.0, 3.0),
            "the render transform must be projected from canonical position"
        );
        assert_eq!(
            app.world().get::<Visibility>(bomb),
            Some(&Visibility::Inherited),
            "a render-child parent must participate in visibility propagation"
        );
        assert!(app.world().get::<InheritedVisibility>(bomb).is_some());
        let world = app.world_mut();
        let mut visuals = world.query::<(&ArenaCannonBombVisual, &ChildOf)>();
        let parents: Vec<_> = visuals
            .iter(world)
            .map(|(_, parent)| parent.parent())
            .collect();
        assert_eq!(parents, vec![bomb]);
    }

    #[test]
    fn arena_preview_layers_include_gameplay_and_preview_cameras() {
        let layers = arena_geometry_render_layers();

        assert!(layers.intersects(&RenderLayers::default()));
        assert!(layers.intersects(&RenderLayers::layer(ARENA_PREVIEW_RENDER_LAYER)));
        assert_ne!(ARENA_PREVIEW_RENDER_LAYER, 0);
    }

    #[test]
    fn radius_support_extends_platform_ground_query_slightly() {
        let platform = arena_definitions()[0].platforms[0];
        let x = platform.center.x + platform.half_extents.x + 0.08;
        assert_eq!(ground_height_at(x, platform.center.y), None);
        assert_eq!(
            ground_height_at_with_radius(x, platform.center.y, 0.4),
            Some(platform.top_y)
        );
        assert_eq!(
            ground_support_at_with_radius(x, platform.center.y, 0.4),
            GroundSupport::Grace(platform.top_y)
        );
        assert_eq!(
            ground_height_at_with_radius(
                platform.center.x + platform.half_extents.x + 0.2,
                platform.center.y,
                0.4,
            ),
            None
        );
    }

    #[test]
    fn platform_side_collision_pushes_out_of_margin_not_top() {
        let platform = PlatformDefinition::new(0.0, 0.0, 1.0, 1.0, ARENA_TOP_Y + 0.4);
        let side = resolve_platform_side_collision_against(
            Vec3::new(1.2, ARENA_TOP_Y, 0.0),
            0.4,
            &platform,
        );
        assert!(side.x > 1.2);

        let top = resolve_platform_side_collision_against(
            Vec3::new(0.0, platform.top_y, 0.0),
            0.4,
            &platform,
        );
        assert_eq!(top, Vec3::new(0.0, platform.top_y, 0.0));
    }

    #[test]
    fn round_pipe_collision_does_not_create_invisible_square_corners() {
        let crank = &arena_definitions()[CRANK_YARD_ARENA_INDEX];
        let pipe_pair = crank.pipe_pair.expect("Crank Yard pipe pair");
        let pipe = pipe_barrier(pipe_pair, pipe_pair.endpoints[0]);
        let corner = Vec3::new(pipe.center.x + 1.1, ARENA_TOP_Y, pipe.center.y + 1.1);
        let side = Vec3::new(pipe.center.x + 0.9, ARENA_TOP_Y, pipe.center.y);
        let landing_approach = Vec3::new(
            pipe.center.x + pipe_pair.collider_radius + FIGHTER_RADIUS * 0.5,
            pipe.top_y - crate::constants::LANDING_SNAP_TOLERANCE * 2.0,
            pipe.center.y,
        );

        assert_eq!(
            resolve_circular_platform_side_collision_against(
                corner,
                FIGHTER_RADIUS,
                &pipe,
                pipe_pair.collider_radius,
                pipe_pair.trigger_radius,
            ),
            corner
        );
        assert!(
            resolve_circular_platform_side_collision_against(
                side,
                FIGHTER_RADIUS,
                &pipe,
                pipe_pair.collider_radius,
                pipe_pair.trigger_radius,
            )
            .x > side.x
        );
        assert_eq!(
            resolve_circular_platform_side_collision_against(
                landing_approach,
                FIGHTER_RADIUS,
                &pipe,
                pipe_pair.collider_radius,
                0.0,
            ),
            landing_approach
        );

        let opening = Vec3::new(pipe.center.x, ARENA_TOP_Y, pipe.center.y);
        assert_ne!(
            resolve_circular_platform_side_collision_against(
                opening,
                FIGHTER_RADIUS,
                &pipe,
                pipe_pair.collider_radius,
                0.0,
            ),
            opening
        );

        let corner_support = ground_support_for_arena_with_radius(
            crank,
            pipe.center.x + pipe_pair.collider_radius * 0.8,
            pipe.center.y + pipe_pair.collider_radius * 0.8,
            0.0,
        );
        assert_ne!(corner_support.height(), Some(pipe.top_y));
        assert_eq!(
            ground_support_for_arena_with_radius(crank, pipe.center.x, pipe.center.y, 0.0,)
                .height(),
            Some(pipe.top_y)
        );
        assert_eq!(
            ground_support_for_arena_with_radius(crank, pipe.center.x + 0.62, pipe.center.y, 0.0,)
                .height(),
            Some(pipe.top_y)
        );
    }

    #[test]
    fn vent_tier_side_collision_opens_at_landing_height() {
        let vent = &arena_definitions()[VENT_SPIRAL_ARENA_INDEX];
        let tier = &vent.platforms[0];
        let approach = Vec3::new(
            4.15,
            ARENA_TOP_Y,
            tier.center.y - tier.half_extents.y - FIGHTER_RADIUS * 0.5,
        );
        assert_ne!(
            tier.resolve_side_collision(
                approach,
                FIGHTER_RADIUS,
                crate::constants::LANDING_SNAP_TOLERANCE,
            ),
            approach
        );

        let landing = Vec3::new(
            approach.x,
            tier.top_y - crate::constants::LANDING_SNAP_TOLERANCE,
            approach.z,
        );
        assert_eq!(
            tier.resolve_side_collision(
                landing,
                FIGHTER_RADIUS,
                crate::constants::LANDING_SNAP_TOLERANCE,
            ),
            landing
        );
    }

    #[test]
    fn raised_walkable_platforms_open_at_landing_height_across_arenas() {
        let platform_cases = [
            (1, 2, "Split Causeway"),
            (2, 0, "Sunstone Steps"),
            (3, 0, "Crank Yard"),
            (4, 0, "Vent Spiral"),
            (8, 0, "Sky Steps"),
        ];

        for (arena_index, platform_index, arena_name) in platform_cases {
            let arena = &arena_definitions()[arena_index];
            let platform = &arena.platforms[platform_index];
            let approach = Vec3::new(
                platform.center.x + platform.half_extents.x + FIGHTER_RADIUS * 0.5,
                platform.top_y - crate::constants::LANDING_SNAP_TOLERANCE - 0.01,
                platform.center.y,
            );
            assert_ne!(
                resolve_platform_side_collision_for_arena(arena, approach, FIGHTER_RADIUS),
                approach,
                "{arena_name} should block below its visible platform top"
            );

            let landing = Vec3::new(
                approach.x,
                platform.top_y - crate::constants::LANDING_SNAP_TOLERANCE,
                approach.z,
            );
            assert_eq!(
                resolve_platform_side_collision_for_arena(arena, landing, FIGHTER_RADIUS),
                landing,
                "{arena_name} should open at landing height"
            );
        }
    }

    #[test]
    fn floor_level_platforms_remain_free_of_side_barriers() {
        for arena_index in [0, 5, 6, 7, 9] {
            let arena = &arena_definitions()[arena_index];
            let platform = &arena.platforms[0];
            let position = Vec3::new(
                platform.center.x - platform.half_extents.x - FIGHTER_RADIUS * 0.5,
                ARENA_TOP_Y,
                platform.center.y,
            );
            assert_eq!(
                resolve_platform_side_collision_for_arena(arena, position, FIGHTER_RADIUS),
                position,
                "{} should not gain a floor-level side wall",
                arena.name
            );
        }
    }

    #[test]
    fn floor_level_platform_side_collision_does_not_block_walkable_extensions() {
        let platform = PlatformDefinition::new(0.0, 0.0, 1.0, 1.0, ARENA_TOP_Y - 0.05);
        let side = resolve_platform_side_collision_against(
            Vec3::new(1.2, ARENA_TOP_Y, 0.0),
            0.4,
            &platform,
        );

        assert_eq!(side, Vec3::new(1.2, ARENA_TOP_Y, 0.0));
    }

    #[test]
    fn arena_hazard_pulse_uses_active_window() {
        assert!(arena_hazard_is_active(0.1, 2.0));
        assert!(!arena_hazard_is_active(1.2, 2.0));
        assert!(arena_hazard_is_active(2.05, 2.0));

        let snare = ArenaHazardDefinition {
            kind: ArenaHazardKind::SnareField,
            center: Vec3::ZERO,
            radius: 1.0,
            pulse_seconds: 2.0,
            phase: 0.0,
        };
        assert!(arena_hazard_is_active_for_kind(1.2, &snare));

        let phased_pulse = ArenaHazardDefinition {
            kind: ArenaHazardKind::PulseVent,
            center: Vec3::ZERO,
            radius: 1.0,
            pulse_seconds: 2.0,
            phase: 1.6,
        };
        assert!(arena_hazard_is_active_for_kind(0.5, &phased_pulse));

        let campfire = ArenaHazardDefinition {
            kind: ArenaHazardKind::Campfire,
            center: Vec3::ZERO,
            radius: 1.0,
            pulse_seconds: 1.4,
            phase: 0.0,
        };
        assert!(arena_hazard_is_active_for_kind(0.1, &campfire));
        assert!(arena_hazard_is_active_for_kind(1.3, &campfire));
    }

    #[test]
    fn arena_hazard_overlap_includes_fighter_radius() {
        let hazard = ArenaHazardDefinition {
            kind: ArenaHazardKind::PulseVent,
            center: Vec3::ZERO,
            radius: 1.0,
            pulse_seconds: 2.0,
            phase: 0.0,
        };
        assert!(arena_hazard_overlaps(
            &hazard,
            Vec3::new(1.0 + FIGHTER_RADIUS * 0.8, 0.0, 0.0),
        ));
        assert!(!arena_hazard_overlaps(
            &hazard,
            Vec3::new(1.0 + FIGHTER_RADIUS * 1.4, 0.0, 0.0),
        ));
    }

    #[test]
    fn raised_vent_hazards_do_not_hit_through_lower_tiers() {
        let hazard = ArenaHazardDefinition {
            kind: ArenaHazardKind::PulseVent,
            center: Vec3::new(0.0, ARENA_TOP_Y + 1.36, 0.0),
            radius: 0.82,
            pulse_seconds: 3.6,
            phase: 0.0,
        };

        assert!(arena_hazard_overlaps(
            &hazard,
            Vec3::new(0.0, ARENA_TOP_Y + 1.3, 0.0)
        ));
        assert!(!arena_hazard_overlaps(
            &hazard,
            Vec3::new(0.0, ARENA_TOP_Y + 0.65, 0.0)
        ));
        assert!(arena_hazard_affects_height(&hazard, hazard.center.y + 1.8));
    }

    #[test]
    fn vent_visual_clock_warns_before_matching_active_window() {
        let cycle = 3.6;
        assert_eq!(vent_charge_visual_amount(1.8, cycle, 0.0), 0.0);
        assert!(vent_charge_visual_amount(3.4, cycle, 0.0) > 0.7);
        assert!(vent_active_visual_amount(0.2, cycle, 0.0) > 0.25);
        assert_eq!(vent_active_visual_amount(1.8, cycle, 0.0), 0.0);
    }

    #[test]
    fn fighter_burn_visual_starts_hot_and_fades_out() {
        let fresh = ArenaFighterBurn::new(ARENA_HAZARD_CAMPFIRE_BURN_SECONDS);
        let ending = ArenaFighterBurn {
            remaining_seconds: SIM_DT_SECONDS,
            duration_seconds: ARENA_HAZARD_CAMPFIRE_BURN_SECONDS,
        };

        assert!(fresh.visual_amount() > 0.7);
        assert!(ending.visual_amount() < 0.15);
    }

    #[test]
    fn arena_hazard_profiles_vary_by_kind() {
        let pulse = arena_hazard_impact_profile(ArenaHazardKind::PulseVent);
        let snare = arena_hazard_impact_profile(ArenaHazardKind::SnareField);
        let bumper = arena_hazard_impact_profile(ArenaHazardKind::BumperNode);
        let campfire = arena_hazard_impact_profile(ArenaHazardKind::Campfire);
        let saw = arena_hazard_impact_profile(ArenaHazardKind::SawBlade);

        assert!(pulse.force_knockdown);
        assert!(!snare.force_knockdown);
        assert!(snare.knockback < pulse.knockback);
        assert!(bumper.knockback > pulse.knockback);
        assert!(campfire.knockback > snare.knockback);
        assert!(campfire.force_knockdown);
        assert!(!campfire.guardable);
        assert_eq!(campfire.reaction_family, ReactionFamilyId::LauncherDown);
        assert!(campfire.reaction.landing_aftermath.is_some());
        assert_eq!(saw.damage, ARENA_HAZARD_SAW_DAMAGE);
        assert!(saw.knockback > campfire.knockback);
        assert!(saw.vertical_knockback > campfire.vertical_knockback);
        assert!(saw.force_knockdown);
        assert!(!saw.guardable);
        assert!(saw.feedback.heavy_spark);
        assert_eq!(saw.reaction_family, ReactionFamilyId::LauncherDown);
        assert!(saw.reaction.landing_aftermath.is_some());
        assert!(arena_hazard_hit_cooldown(ArenaHazardKind::SawBlade) < 0.7);
        assert!(arena_hazard_hit_cooldown(ArenaHazardKind::SnareField) < 1.0);
    }

    #[test]
    fn saw_knockback_always_points_away_from_the_blade() {
        let center = Vec3::new(-3.1, ARENA_TOP_Y, 0.0);
        assert_eq!(
            saw_knockback_direction(center + Vec3::Z, center, -Vec3::Z),
            Vec3::Z
        );
        assert_eq!(saw_knockback_direction(center, center, Vec3::Z), -Vec3::X);
    }

    #[test]
    fn crank_pipe_accepts_a_grounded_fighter_or_descending_jump() {
        let pipe_pair = arena_definitions()[CRANK_YARD_ARENA_INDEX]
            .pipe_pair
            .expect("Crank Yard pipe pair");
        let center = pipe_pair.endpoints[0];
        let position = Vec3::new(center.x, pipe_pair.top_y, center.y);
        let grounded_motor = FighterMotor {
            grounded: true,
            ..default()
        };
        let airborne_motor = FighterMotor {
            grounded: false,
            ..default()
        };
        let descending_motor = FighterMotor {
            velocity: Vec3::NEG_Y,
            grounded: false,
            ..default()
        };
        let ascending_motor = FighterMotor {
            velocity: Vec3::Y,
            grounded: false,
            ..default()
        };

        assert_eq!(
            pipe_entry_endpoint(pipe_pair, position, &grounded_motor, FighterAction::Idle),
            Some(0)
        );
        assert_eq!(
            pipe_entry_endpoint(pipe_pair, position, &airborne_motor, FighterAction::Idle),
            None
        );
        assert_eq!(
            pipe_entry_endpoint(
                pipe_pair,
                position + Vec3::Y * 0.35,
                &descending_motor,
                FighterAction::Jumping,
            ),
            Some(0)
        );
        assert_eq!(
            pipe_entry_endpoint(
                pipe_pair,
                position + Vec3::Y * 0.35,
                &ascending_motor,
                FighterAction::Jumping,
            ),
            None
        );
        assert_eq!(
            pipe_entry_endpoint(
                pipe_pair,
                position,
                &grounded_motor,
                FighterAction::HeavyAttack
            ),
            None
        );
    }

    #[test]
    fn crank_pipe_transit_sinks_then_emerges_at_the_other_endpoint() {
        let pipe_pair = arena_definitions()[CRANK_YARD_ARENA_INDEX]
            .pipe_pair
            .expect("Crank Yard pipe pair");
        let base_scale = Vec3::splat(1.2);
        let entering = pipe_transit_sample(
            pipe_pair,
            0,
            1,
            ElapsedTicks::from_ticks(seconds_to_ticks_ceil(PIPE_ENTER_SECONDS * 0.5)),
            pipe_pair.top_y,
            base_scale,
        );
        assert_eq!(entering.position.x, pipe_pair.endpoints[0].x);
        assert!(entering.position.y < pipe_pair.top_y);
        assert!(entering.scale.x < base_scale.x);

        let exiting = pipe_transit_sample(
            pipe_pair,
            0,
            1,
            ElapsedTicks::from_ticks(seconds_to_ticks_ceil(
                PIPE_ENTER_SECONDS + PIPE_TRAVEL_SECONDS + PIPE_EXIT_SECONDS * 0.5,
            )),
            pipe_pair.top_y,
            base_scale,
        );
        assert_eq!(exiting.position.x, pipe_pair.endpoints[1].x);
        assert!(exiting.position.y < pipe_pair.top_y);

        let complete = pipe_transit_sample(
            pipe_pair,
            0,
            1,
            ElapsedTicks::from_ticks(PIPE_TRANSIT_END_TICKS),
            pipe_pair.top_y,
            base_scale,
        );
        assert!(complete.complete);
        assert_eq!(complete.position.y, pipe_pair.top_y);
        assert_eq!(complete.scale, base_scale);
    }

    #[test]
    fn pipe_transit_base_scale_ignores_render_owned_transform_pulses() {
        let mut stats = FighterStats::default();
        assert_eq!(fighter_pipe_base_scale(&stats), Vec3::ONE);

        stats.item_giant_timer = TickTimer::from_ticks(1);
        assert_eq!(
            fighter_pipe_base_scale(&stats),
            Vec3::splat(stats.item_size_multiplier())
        );
    }

    #[test]
    fn arena_runtime_snapshot_round_trips_private_resources_exactly() {
        let arena_index = 3;
        let active = ActiveArena::new(arena_index);
        let mut world = World::new();
        world.insert_resource(active);

        let mut hazard = ArenaHazardState::new(arena_index, active.definition().hazards.len());
        hazard.elapsed = ElapsedTicks::from_ticks(321);
        hazard.hit_cooldowns[0][0] = TickTimer::from_ticks(8);
        hazard.hit_cooldowns[1][0] = TickTimer::from_ticks(5);
        hazard.hit_cooldowns[1][3] = TickTimer::from_ticks(13);
        hazard.crank_saws_stopped = true;
        hazard.crank_lever_toggle_cooldown = TickTimer::from_ticks(17);
        world.insert_resource(hazard);

        let mut pipes = ArenaPipeState::new(arena_index);
        pipes.fighters[0] = FighterPipeState::Ready {
            candidate: Some(1),
            dwell_ticks: 7,
            cooldown: TickTimer::from_ticks(2),
        };
        pipes.fighters[2] = FighterPipeState::Transit {
            source: 0,
            destination: 1,
            elapsed: ElapsedTicks::from_ticks(19),
            entry_y: 1.25,
            base_scale: Vec3::splat(1.5),
        };
        world.insert_resource(pipes);
        world.insert_resource(PowderKegCannonState {
            arena_index,
            fire_timer: TickTimer::from_ticks(29),
            next_cannon: 1,
        });

        let snapshot = capture_arena_runtime_snapshot(&world).unwrap();
        assert_eq!(snapshot.hazard_clock_ticks, 321);
        assert_eq!(snapshot.per_fighter_hazard_cooldowns, [8, 0, 0, 13]);
        assert_eq!(
            snapshot.logical_device_flags,
            ARENA_DEVICE_CRANK_SAWS_STOPPED
        );

        world.insert_resource(ArenaHazardState::new(arena_index, 0));
        world.insert_resource(ArenaPipeState::new(arena_index));
        world.insert_resource(PowderKegCannonState::new(arena_index));
        let plan = prepare_arena_runtime_restore(&world, &snapshot).unwrap();
        commit_arena_runtime_restore(&mut world, plan);
        assert_eq!(capture_arena_runtime_snapshot(&world).unwrap(), snapshot);
    }

    #[test]
    fn arena_runtime_restore_rejects_hostile_payload_and_pipe_state() {
        let arena_index = 3;
        let active = ActiveArena::new(arena_index);
        let mut world = World::new();
        world.insert_resource(active);
        world.insert_resource(ArenaHazardState::new(
            arena_index,
            active.definition().hazards.len(),
        ));
        world.insert_resource(ArenaPipeState::new(arena_index));
        world.insert_resource(PowderKegCannonState::new(arena_index));
        let snapshot = capture_arena_runtime_snapshot(&world).unwrap();

        let mut padding = snapshot.clone();
        padding.payload[ARENA_PAYLOAD_BYTES - 1] = 1;
        assert!(matches!(
            prepare_arena_runtime_restore(&world, &padding),
            Err(ArenaRuntimeSnapshotError::NonCanonicalPayloadPadding)
        ));

        let mut aggregate = snapshot.clone();
        aggregate.per_fighter_hazard_cooldowns[0] = 1;
        assert!(matches!(
            prepare_arena_runtime_restore(&world, &aggregate),
            Err(ArenaRuntimeSnapshotError::InconsistentHazardAggregate { fighter: 0, .. })
        ));

        let mut pipe = snapshot;
        pipe.pipes[0].flags = 0x80;
        assert!(matches!(
            prepare_arena_runtime_restore(&world, &pipe),
            Err(ArenaRuntimeSnapshotError::InvalidPipeFlags(0x80))
        ));
    }

    #[test]
    fn crank_yard_has_no_harmless_static_saw_decoy() {
        assert!(
            CRANK_ASSET_PROPS
                .iter()
                .all(|prop| prop.name != "Crank yard center saw")
        );
    }

    #[test]
    fn arena_hazard_markers_telegraph_active_wave_peaks() {
        assert!(
            arena_hazard_marker_scale(ArenaHazardKind::PulseVent, 1.0)
                > arena_hazard_marker_scale(ArenaHazardKind::PulseVent, -1.0)
        );
        assert!(
            arena_hazard_marker_scale(ArenaHazardKind::BumperNode, 1.0)
                > arena_hazard_marker_scale(ArenaHazardKind::BumperNode, 0.0)
        );
        assert!(
            arena_hazard_marker_scale(ArenaHazardKind::SnareField, 1.0)
                > arena_hazard_marker_scale(ArenaHazardKind::SnareField, -1.0)
        );
        assert!(
            arena_hazard_marker_scale(ArenaHazardKind::Campfire, 1.0)
                > arena_hazard_marker_scale(ArenaHazardKind::Campfire, -1.0)
        );
    }

    #[test]
    fn arena_background_wallpapers_use_authored_three_to_two_aspect() {
        for arena in arena_definitions() {
            let size = arena_background_wallpaper_size(arena.background);
            assert!((size.x / size.y - 1.5).abs() < 0.001, "{}", arena.name);
            assert!(size.x > ARENA_RADIUS * 2.0, "{}", arena.name);

            let camera_transform =
                Transform::from_translation(arena.camera_offset).looking_at(Vec3::Y * 0.6, Vec3::Y);
            let transform =
                arena_background_wallpaper_transform(arena.background, &camera_transform);
            let to_camera = (arena.camera_offset - transform.translation).normalize();
            let normal = transform.rotation * Vec3::Z;
            assert!(
                (transform.translation.distance(arena.camera_offset) - arena.background.distance)
                    .abs()
                    < 0.001,
                "{}",
                arena.name
            );
            assert!(normal.dot(to_camera) > 0.999, "{}", arena.name);
        }
    }

    #[test]
    fn mini_arena_props_cover_stage_variants() {
        for index in 0..arena_definitions().len() {
            let props = arena_asset_props(index);
            let expected_minimum = match index {
                1 | 2 => 4,
                CRANK_YARD_ARENA_INDEX => 4,
                VENT_SPIRAL_ARENA_INDEX => 1,
                7 => 3,
                _ => 5,
            };
            assert!(props.len() >= expected_minimum);
            assert!(props.iter().all(|prop| prop.file.ends_with(".glb")));
            assert!(props.iter().all(|prop| prop.scale > 0.0));
            assert!(props.iter().all(|prop| prop.y >= ARENA_TOP_Y - 0.6));
            #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
            assert!(props.iter().all(|prop| {
                std::path::Path::new("assets")
                    .join(arena_prop_asset_path(prop.file))
                    .is_file()
            }));
        }
    }

    #[test]
    fn vent_spiral_uses_one_non_overlapping_static_reactor_mesh() {
        let props = arena_asset_props(VENT_SPIRAL_ARENA_INDEX);
        assert_eq!(props.len(), 1);
        assert_eq!(props[0].file, "tower/tower-round-crystals.glb");
        assert_eq!(props[0].scale, 3.1);
    }

    #[test]
    fn arena_render_depth_bias_separates_coplanar_surfaces() {
        for arena in arena_definitions() {
            for index in 1..arena.ground_shapes.len() {
                assert!(arena_ground_depth_bias(index) > arena_ground_depth_bias(index - 1));
            }
            for index in 1..arena.platforms.len() {
                assert!(arena_platform_depth_bias(index) > arena_platform_depth_bias(index - 1));
            }
            if !arena.ground_shapes.is_empty() && !arena.platforms.is_empty() {
                assert!(
                    arena_ground_depth_bias(arena.ground_shapes.len() - 1)
                        < arena_platform_depth_bias(0),
                    "{} ground surfaces must render behind platform surfaces",
                    arena.name
                );
            }
        }
    }

    #[test]
    fn arena_props_clear_the_floor_contact_plane() {
        let prop = CROWN_ASSET_PROPS[0];
        let transform = prop.transform();

        assert!((transform.translation.y - prop.y - ARENA_PROP_SURFACE_CLEARANCE).abs() < 0.001);
    }

    #[test]
    fn dry_arena_props_do_not_use_river_assets() {
        for index in [1, 2] {
            assert!(
                arena_asset_props(index)
                    .iter()
                    .all(|prop| !prop.file.contains("river"))
            );
        }
    }

    #[test]
    fn arena_footprints_support_every_fighter_spawn() {
        for arena in arena_definitions() {
            assert!(!arena.ground_shapes.is_empty());
            for spawn in arena.spawn_points {
                assert!(
                    arena_position_is_firm_supported(arena, spawn.x, spawn.z),
                    "{} spawn {spawn:?} must be supported",
                    arena.name
                );
            }
        }
    }

    #[test]
    fn arena_footprints_support_items_and_hazards() {
        for arena in arena_definitions() {
            for anchor in arena.item_anchors {
                assert!(
                    arena_position_is_firm_supported(arena, anchor.position.x, anchor.position.z),
                    "{} item at {:?} must be supported",
                    arena.name,
                    anchor.position
                );
            }
            for hazard in arena.hazards {
                assert!(
                    arena_position_is_firm_supported(arena, hazard.center.x, hazard.center.z),
                    "{} hazard at {:?} must be supported",
                    arena.name,
                    hazard.center
                );
            }
        }
    }

    #[test]
    fn every_rendered_prop_has_an_explicit_collision_policy() {
        for arena_index in 1..arena_definitions().len() {
            for prop in arena_asset_props(arena_index) {
                let _ = prop_collision_profile(prop.file);
            }
        }
    }

    #[test]
    fn snare_garden_has_no_hedge_or_bush_props() {
        assert!(
            SNARE_GARDEN_ASSET_PROPS
                .iter()
                .all(|prop| !prop.file.contains("hedge") && !prop.file.contains("bush"))
        );
    }

    #[test]
    fn champions_court_objects_generate_shared_prop_barriers() {
        let barriers = champions_court_collision_barriers();
        let rectangle_count = barriers
            .iter()
            .filter(|barrier| {
                matches!(
                    barrier.definition.footprint,
                    BarrierFootprint::Rectangle { .. }
                )
            })
            .count();
        let circle_count = barriers.len() - rectangle_count;
        let one_way_count = barriers
            .iter()
            .filter(|barrier| barrier.behavior == PropBarrierBehavior::OneWayTop)
            .count();

        assert_eq!(barriers.len(), 91);
        assert_eq!((rectangle_count, circle_count), (78, 13));
        assert_eq!((barriers.len() - one_way_count, one_way_count), (69, 22));
        assert!(barriers.iter().any(|barrier| {
            barrier.definition.center.distance(Vec2::ZERO) < 0.01
                && barrier.definition.top_y > ARENA_TOP_Y
        }));
        assert!(
            barriers
                .iter()
                .any(|barrier| barrier.behavior == PropBarrierBehavior::OneWayTop)
        );
    }

    fn champions_court_collision_words(barrier: &WorldPropBarrier) -> [u32; 8] {
        let behavior = u32::from(barrier.behavior == PropBarrierBehavior::OneWayTop);
        match barrier.definition.footprint {
            BarrierFootprint::Circle { radius } => [
                0,
                behavior,
                barrier.definition.center.x.to_bits(),
                barrier.definition.center.y.to_bits(),
                barrier.definition.top_y.to_bits(),
                radius.to_bits(),
                0,
                0,
            ],
            BarrierFootprint::Rectangle { half_extents, yaw } => [
                1,
                behavior,
                barrier.definition.center.x.to_bits(),
                barrier.definition.center.y.to_bits(),
                barrier.definition.top_y.to_bits(),
                half_extents.x.to_bits(),
                half_extents.y.to_bits(),
                yaw.to_bits(),
            ],
        }
    }

    #[test]
    fn champions_court_prebake_matches_frozen_record_fingerprint() {
        let words: Vec<_> = champions_court_collision_barriers()
            .iter()
            .flat_map(champions_court_collision_words)
            .collect();
        assert_eq!(words.len(), 91 * 8);
        assert_eq!(
            crate::canonical_math::fnv1a64_words(&words),
            CHAMPIONS_COURT_COLLISION_FNV1A64
        );
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn champions_court_prebake_matches_every_v3_reference_record_bit() {
        let map: ChampionsCourtRon = ron::from_str(include_str!("../arts/champions_court.ron"))
            .expect("embedded Champion's Court RON should parse");
        let mut reference = Vec::new();

        for object in &map.instances {
            let transform = Transform::from_xyz(
                object.position.0,
                ARENA_TOP_Y + object.position.1 + ARENA_PROP_SURFACE_CLEARANCE,
                object.position.2,
            )
            .with_rotation(Quat::from_rotation_y(object.rotation_y.to_radians()))
            .with_scale(Vec3::new(object.scale.0, object.scale.1, object.scale.2));
            append_champions_object_barriers(&map.assets, object, transform, &mut reference);
        }

        for prefab_instance in &map.prefab_instances {
            let Some(objects) = map.prefabs.get(&prefab_instance.prefab) else {
                continue;
            };
            for object in objects {
                append_champions_object_barriers(
                    &map.assets,
                    object,
                    champions_prefab_object_transform(prefab_instance, object),
                    &mut reference,
                );
            }
        }

        assert_eq!(reference.len(), CHAMPIONS_COURT_COLLISION_BARRIERS.len());
        for (index, (actual, expected)) in CHAMPIONS_COURT_COLLISION_BARRIERS
            .iter()
            .zip(&reference)
            .enumerate()
        {
            assert_eq!(
                champions_court_collision_words(actual),
                champions_court_collision_words(expected),
                "Champion's Court barrier {index} changed from v3 reference"
            );
        }
    }

    #[test]
    fn hollow_structure_center_stays_open_while_posts_block() {
        let sunstone = &arena_definitions()[2];
        let structure = SUNSTONE_ASSET_PROPS[0];
        let inside = Vec3::new(structure.x, ARENA_TOP_Y, structure.z);
        assert_eq!(
            resolve_platform_side_collision_for_arena(sunstone, inside, FIGHTER_RADIUS,),
            inside
        );

        let post = structure
            .collision_barriers()
            .find(|barrier| barrier.behavior == PropBarrierBehavior::Solid)
            .expect("wood structure should have solid posts");
        let post_position = Vec3::new(
            post.definition.center.x,
            ARENA_TOP_Y,
            post.definition.center.y,
        );
        assert_ne!(
            resolve_platform_side_collision_for_arena(sunstone, post_position, FIGHTER_RADIUS,),
            post_position
        );
    }

    #[test]
    fn rotated_ground_rectangles_use_local_shape_axes() {
        let shape = ArenaGroundShape::rectangle(0.0, 0.0, 3.0, 0.5, PI * 0.5, 1.2);

        assert_eq!(
            ground_shape_support(&shape, 0.0, 2.5, 0.0),
            Some(GroundSupport::Firm(1.2))
        );
        assert_eq!(ground_shape_support(&shape, 2.5, 0.0, 0.0), None);
    }

    #[test]
    fn unsupported_floor_tiles_are_not_renderable() {
        assert!(!floor_tile_is_firm_supported(
            0,
            ARENA_RADIUS + 4.0,
            ARENA_RADIUS + 4.0
        ));
    }

    #[test]
    fn champions_court_authorship_parses() {
        let map = load_champions_court_map().expect("champions court RON should parse");
        assert_eq!(map.map.tile_size, 2.0);
        assert!(map.assets.contains_key("floor"));
        assert!(!map.floor_shapes.is_empty());
        assert!(!map.instances.is_empty());
        assert!(!map.prefab_instances.is_empty());
    }

    #[cfg(not(all(
        feature = "dev-hot-reload",
        not(feature = "shipping"),
        not(target_arch = "wasm32")
    )))]
    #[test]
    fn immutable_champions_court_does_not_consult_the_working_directory() {
        let missing = std::env::temp_dir().join("afc-definitely-missing-champions-court.ron");
        let map = load_champions_court_map_from_path(&missing)
            .expect("the embedded Champions Court should parse without a loose file");
        assert_eq!(map.map.tile_size, 2.0);
        assert!(map.assets.contains_key("floor"));
        assert!(!map.instances.is_empty());
    }

    #[test]
    fn champions_asset_paths_use_runtime_asset_root() {
        let assets = HashMap::from([("floor".to_string(), "floor.glb".to_string())]);
        assert_eq!(
            champions_runtime_asset_path(&assets, "floor"),
            Some("arena/kenney_mini_arena/floor.glb".to_string())
        );
        assert_eq!(champions_runtime_asset_path(&assets, "missing"), None);
    }

    #[test]
    fn champions_floor_shapes_expand_octagons_and_even_rectangles() {
        let octagon = ChampionsCourtFloorShape {
            id: "test_octagon".to_string(),
            kind: "filled_octagon".to_string(),
            asset: "floor".to_string(),
            center: (0, 0),
            radius_tiles: 2,
            inner_radius_tiles: 0,
            outer_radius_tiles: 0,
            size_tiles: (0, 0),
            y: 0.0,
            rotation_y: 0.0,
        };
        let octagon_tiles = champions_floor_shape_tiles(&octagon);
        assert!(octagon_tiles.contains(&Vec2::ZERO));
        assert!(octagon_tiles.contains(&Vec2::new(2.0, 0.0)));
        assert!(!octagon_tiles.contains(&Vec2::new(2.0, 2.0)));

        let rectangle = ChampionsCourtFloorShape {
            id: "test_rect".to_string(),
            kind: "rectangle".to_string(),
            asset: "floor_detail".to_string(),
            center: (0, 0),
            radius_tiles: 0,
            inner_radius_tiles: 0,
            outer_radius_tiles: 0,
            size_tiles: (4, 2),
            y: 0.0,
            rotation_y: 0.0,
        };
        let rectangle_tiles = champions_floor_shape_tiles(&rectangle);
        assert_eq!(rectangle_tiles.len(), 8);
        assert!(rectangle_tiles.contains(&Vec2::new(-1.5, -0.5)));
        assert!(rectangle_tiles.contains(&Vec2::new(1.5, 0.5)));

        let far_rectangle = ChampionsCourtFloorShape {
            center: (64, 64),
            ..rectangle
        };
        assert!(champions_floor_shape_render_positions(&far_rectangle, 2.0, 0).is_empty());
    }

    #[test]
    fn champions_prefab_transform_combines_parent_and_child() {
        let prefab_instance = ChampionsCourtPrefabInstance {
            id: "rotated_prefab".to_string(),
            prefab: "weapon_corner".to_string(),
            position: (10.0, 1.0, 0.0),
            rotation_y: 90.0,
            scale: (2.0, 1.0, 2.0),
        };
        let object = ChampionsCourtObject {
            id: "child".to_string(),
            asset: "weapon_spear".to_string(),
            position: (1.0, 0.5, 0.0),
            rotation_y: 30.0,
            scale: (0.5, 0.5, 0.5),
        };

        let transform = champions_prefab_object_transform(&prefab_instance, &object);
        assert!((transform.translation.x - 10.0).abs() < 0.001);
        assert!(
            (transform.translation.y - (ARENA_TOP_Y + 1.5 + ARENA_PROP_SURFACE_CLEARANCE)).abs()
                < 0.001
        );
        assert!((transform.translation.z + 2.0).abs() < 0.001);
        assert_eq!(transform.scale, Vec3::new(1.0, 0.5, 1.0));
    }
}
