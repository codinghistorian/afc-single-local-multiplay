//! Opt-in runtime diagnostics and repeatable performance scenarios.
//!
//! Enabling `perf` only adds instrumentation. A gameplay workload is selected
//! explicitly with `AFC_PERF_SCENARIO`; without that variable the game behaves
//! normally and only the lightweight Bevy diagnostics are enabled.

#[cfg(feature = "perf-alloc")]
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use std::{
    io,
    path::{Path, PathBuf},
};

use bevy::app::AppExit;
use bevy::asset::LoadedUntypedAsset;
use bevy::camera::NormalizedRenderTarget;
use bevy::diagnostic::{
    DiagnosticsStore, EntityCountDiagnosticsPlugin, FrameTimeDiagnosticsPlugin,
    LogDiagnosticsPlugin,
};
use bevy::ecs::entity::Entities;
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use bevy::render::diagnostic::RenderDiagnosticsPlugin;
use bevy::render::{
    Render, RenderApp, RenderSystems,
    camera::ExtractedCamera,
    extract_resource::{ExtractResource, ExtractResourcePlugin},
    render_resource::TextureViewId,
    renderer::render_system,
    view::{ExtractedWindows, ViewTarget},
};
use bevy::scene::{SceneInstance, SceneSpawner};
use bevy::window::{PresentMode, PrimaryWindow};
use bevy::winit::{UpdateMode, WinitSettings};

use crate::arena::{ArenaGeometry, ArenaHazardMarker, ArenaScene, ArenaSceneReadyMarker};
#[cfg(test)]
use crate::arena_defs::CRANK_YARD_ARENA_INDEX;
use crate::arena_defs::{
    ActiveArena, ArenaHazardKind, BUMPER_ALLEY_ARENA_INDEX, arena_definitions,
};
use crate::bee_skills::ActiveBeeSkill;
use crate::bot::start_bot_combat_ai;
use crate::chick_skills::ActiveChickSkill;
use crate::components::{
    BotBehaviorMode, BotBrain, Controller, Fighter, Hitbox, LocalInputAssignment, ParticipantKind,
    SimPosition,
};
use crate::game_state::{
    DEFAULT_REPLAY_SEED, LocalSetup, MatchPhase, MatchRules, MatchState, RulePreset,
};
use crate::items::ArenaItem;
use crate::penguin_skills::{ActivePenguinSkill, ActivePenguinSurface};
use crate::specials::ActiveSpecial;
use crate::user_mode::UserModeState;

/// Default interval between diagnostic snapshots written to the application log.
pub const DEFAULT_LOG_INTERVAL: Duration = Duration::from_secs(2);

const DEFAULT_WARMUP_SECONDS: f64 = 30.0;
const DEFAULT_STRESS_SECONDS: f64 = 300.0;
const DEFAULT_SOAK_SECONDS: f64 = 600.0;
const PERFORMANCE_RESULT_SCHEMA_VERSION: u64 = 6;
const MAP_SWITCH_COUNT: usize = 100;
const MAP_WARM_PRECYCLE_SWITCH_COUNT: usize = 10;
const MAP_CYCLE_PRELOAD_FOLDERS: [&str; 3] = ["arena", "backgrounds", "music/bgm"];
const MAP_ALIGNED_CHECKPOINT_COUNT: usize = 11;
const MAP_ALIGNED_TAIL_CHECKPOINTS: usize = 4;
const PERF_STOCKS: i32 = 1_000_000;
const READINESS_TIMEOUT: Duration = Duration::from_secs(30);
const MIN_COMBAT_ACTIVITY_PER_OWNER: usize = 1;
const MAX_RESOURCE_GROWTH_SAMPLES: usize = 64;
const RESOURCE_GROWTH_HISTORY_SAMPLES: usize = 48;
const RESOURCE_GROWTH_TAIL_SAMPLES: usize =
    MAX_RESOURCE_GROWTH_SAMPLES - RESOURCE_GROWTH_HISTORY_SAMPLES;
const RESOURCE_PLATEAU_WINDOW_SECONDS: f64 = 60.0;
const FRAME_P99_BUDGET_MS: f64 = 16.67;
const CPU_MEAN_BUDGET_MS: f64 = 8.33;
const RSS_PLATEAU_RANGE_MIB: f64 = 8.0;
const RSS_PLATEAU_SLOPE_MIB_PER_MINUTE: f64 = 2.0;
const LIVE_BYTES_PLATEAU_RANGE_MIB: f64 = 1.0;
const LIVE_BYTES_PLATEAU_SLOPE_MIB_PER_MINUTE: f64 = 0.25;
const VENT_SPIRAL_ARENA_INDEX: usize = 4;
#[cfg(any(feature = "perf-alloc", test))]
const PEAK_PHASE_MASK: u64 = 0b11;
#[cfg(any(feature = "perf-alloc", test))]
const PEAK_PHASE_INACTIVE: u64 = 0;
#[cfg(any(feature = "perf-alloc", test))]
const PEAK_PHASE_OPENING: u64 = 1;
#[cfg(any(feature = "perf-alloc", test))]
const PEAK_PHASE_ACTIVE: u64 = 2;
#[cfg(any(feature = "perf-alloc", test))]
const PEAK_PHASE_CLOSING: u64 = 3;
type ArenaRenderIdentity = (usize, u64);

#[cfg(feature = "perf-alloc")]
static ALLOCATION_COUNT: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "perf-alloc")]
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "perf-alloc")]
static LIVE_BYTES: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "perf-alloc")]
static PEAK_LIVE_BYTES: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "perf-alloc")]
static ALLOCATION_MEASUREMENT_CONTROL: AtomicU64 = AtomicU64::new(PEAK_PHASE_INACTIVE);
#[cfg(feature = "perf-alloc")]
static ALLOCATION_MUTATORS_IN_FLIGHT: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "perf-alloc")]
static MEASURED_PEAK_LIVE_BYTES: AtomicU64 = AtomicU64::new(0);

/// Read-only counting-allocator counters used by opt-in profiling surfaces.
///
/// Snapshots do not reset process-global counters, so independent profilers can
/// take before/after samples without changing the existing graphical harness.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AllocationSnapshot {
    pub allocation_count: u64,
    pub allocated_bytes: u64,
    pub live_bytes: u64,
    pub peak_live_bytes: u64,
}

impl AllocationSnapshot {
    pub fn delta_since(self, earlier: Self) -> AllocationDelta {
        AllocationDelta {
            allocation_count: self
                .allocation_count
                .saturating_sub(earlier.allocation_count),
            allocated_bytes: self.allocated_bytes.saturating_sub(earlier.allocated_bytes),
            live_bytes_before: earlier.live_bytes,
            live_bytes_after: self.live_bytes,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AllocationDelta {
    pub allocation_count: u64,
    pub allocated_bytes: u64,
    pub live_bytes_before: u64,
    pub live_bytes_after: u64,
}

/// Captures the allocator counters without allocating or mutating them.
pub fn allocation_snapshot() -> AllocationSnapshot {
    #[cfg(feature = "perf-alloc")]
    {
        return AllocationSnapshot {
            allocation_count: ALLOCATION_COUNT.load(Ordering::Relaxed),
            allocated_bytes: ALLOCATED_BYTES.load(Ordering::Relaxed),
            live_bytes: LIVE_BYTES.load(Ordering::Relaxed),
            peak_live_bytes: PEAK_LIVE_BYTES.load(Ordering::Relaxed),
        };
    }

    #[cfg(not(feature = "perf-alloc"))]
    AllocationSnapshot::default()
}

#[cfg(any(feature = "perf-alloc", test))]
fn enter_allocation_mutation(state: &AtomicU64, in_flight: &AtomicU64) -> bool {
    loop {
        let before = state.load(Ordering::Acquire);
        let phase = before & PEAK_PHASE_MASK;
        if matches!(phase, PEAK_PHASE_OPENING | PEAK_PHASE_CLOSING) {
            std::hint::spin_loop();
            continue;
        }
        in_flight.fetch_add(1, Ordering::AcqRel);
        let after = state.load(Ordering::Acquire);
        if before == after {
            return phase == PEAK_PHASE_ACTIVE;
        }
        in_flight.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(any(feature = "perf-alloc", test))]
fn leave_allocation_mutation(in_flight: &AtomicU64) {
    in_flight.fetch_sub(1, Ordering::Release);
}

#[cfg(any(feature = "perf-alloc", test))]
fn publish_measured_peak(contributes: bool, measured_peak: &AtomicU64, live: u64) {
    if contributes {
        measured_peak.fetch_max(live, Ordering::AcqRel);
    }
}

#[cfg(any(feature = "perf-alloc", test))]
fn enter_peak_measurement_transition(
    control: &AtomicU64,
    expected_phase: u64,
    transition_phase: u64,
) -> u64 {
    loop {
        let current = control.load(Ordering::Acquire);
        assert_eq!(
            current & PEAK_PHASE_MASK,
            expected_phase,
            "allocation measurement phase transition is not reentrant"
        );
        let next = current
            .checked_add(1)
            .expect("allocation measurement generation exhausted");
        debug_assert_eq!(next & PEAK_PHASE_MASK, transition_phase);
        if control
            .compare_exchange(current, next, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            return next;
        }
    }
}

#[cfg(any(feature = "perf-alloc", test))]
fn peak_mutations_drained(in_flight: &AtomicU64) -> bool {
    in_flight.load(Ordering::Acquire) == 0
}

#[cfg(feature = "perf-alloc")]
fn wait_for_peak_mutations(in_flight: &AtomicU64) {
    while !peak_mutations_drained(in_flight) {
        std::hint::spin_loop();
    }
}

fn begin_allocation_measurement() -> AllocationSnapshot {
    #[cfg(feature = "perf-alloc")]
    {
        // The transition state prevents a new allocator mutator from crossing
        // the reset/baseline boundary. Any mutator that observed the prior
        // stable state must drain before the measurement becomes active.
        let opening = enter_peak_measurement_transition(
            &ALLOCATION_MEASUREMENT_CONTROL,
            PEAK_PHASE_INACTIVE,
            PEAK_PHASE_OPENING,
        );
        wait_for_peak_mutations(&ALLOCATION_MUTATORS_IN_FLIGHT);
        MEASURED_PEAK_LIVE_BYTES.store(0, Ordering::SeqCst);
        let live_bytes = LIVE_BYTES.load(Ordering::SeqCst);
        MEASURED_PEAK_LIVE_BYTES.fetch_max(live_bytes, Ordering::SeqCst);
        let snapshot = AllocationSnapshot {
            allocation_count: ALLOCATION_COUNT.load(Ordering::SeqCst),
            allocated_bytes: ALLOCATED_BYTES.load(Ordering::SeqCst),
            live_bytes,
            peak_live_bytes: MEASURED_PEAK_LIVE_BYTES.load(Ordering::SeqCst),
        };
        ALLOCATION_MEASUREMENT_CONTROL.store(opening + 1, Ordering::Release);
        return snapshot;
    }

    #[cfg(not(feature = "perf-alloc"))]
    AllocationSnapshot::default()
}

#[cfg(feature = "perf-alloc")]
fn end_allocation_measurement() -> AllocationSnapshot {
    // Closing the epoch first prevents new mutations from entering. A
    // mutation that already proved the active state remains registered
    // until it has published its local post-increment live-byte high.
    let closing = enter_peak_measurement_transition(
        &ALLOCATION_MEASUREMENT_CONTROL,
        PEAK_PHASE_ACTIVE,
        PEAK_PHASE_CLOSING,
    );
    wait_for_peak_mutations(&ALLOCATION_MUTATORS_IN_FLIGHT);
    let snapshot = AllocationSnapshot {
        allocation_count: ALLOCATION_COUNT.load(Ordering::SeqCst),
        allocated_bytes: ALLOCATED_BYTES.load(Ordering::SeqCst),
        live_bytes: LIVE_BYTES.load(Ordering::SeqCst),
        peak_live_bytes: MEASURED_PEAK_LIVE_BYTES.load(Ordering::SeqCst),
    };
    ALLOCATION_MEASUREMENT_CONTROL.store(closing + 1, Ordering::Release);
    snapshot
}

#[cfg(feature = "perf-alloc")]
struct CountingAllocator;

#[cfg(feature = "perf-alloc")]
#[global_allocator]
static GLOBAL_ALLOCATOR: CountingAllocator = CountingAllocator;

#[cfg(feature = "perf-alloc")]
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let contributes = enter_allocation_mutation(
            &ALLOCATION_MEASUREMENT_CONTROL,
            &ALLOCATION_MUTATORS_IN_FLIGHT,
        );
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            record_allocation(layout.size() as u64, contributes);
        }
        leave_allocation_mutation(&ALLOCATION_MUTATORS_IN_FLIGHT);
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let contributes = enter_allocation_mutation(
            &ALLOCATION_MEASUREMENT_CONTROL,
            &ALLOCATION_MUTATORS_IN_FLIGHT,
        );
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            record_allocation(layout.size() as u64, contributes);
        }
        leave_allocation_mutation(&ALLOCATION_MUTATORS_IN_FLIGHT);
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        let _contributes = enter_allocation_mutation(
            &ALLOCATION_MEASUREMENT_CONTROL,
            &ALLOCATION_MUTATORS_IN_FLIGHT,
        );
        unsafe { System.dealloc(pointer, layout) };
        LIVE_BYTES.fetch_sub(layout.size() as u64, Ordering::Relaxed);
        leave_allocation_mutation(&ALLOCATION_MUTATORS_IN_FLIGHT);
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let contributes = enter_allocation_mutation(
            &ALLOCATION_MEASUREMENT_CONTROL,
            &ALLOCATION_MUTATORS_IN_FLIGHT,
        );
        let next_pointer = unsafe { System.realloc(pointer, layout, new_size) };
        if !next_pointer.is_null() {
            ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
            ALLOCATED_BYTES.fetch_add(new_size as u64, Ordering::Relaxed);
            if new_size >= layout.size() {
                add_live_bytes((new_size - layout.size()) as u64, contributes);
            } else {
                LIVE_BYTES.fetch_sub((layout.size() - new_size) as u64, Ordering::Relaxed);
            }
        }
        leave_allocation_mutation(&ALLOCATION_MUTATORS_IN_FLIGHT);
        next_pointer
    }
}

#[cfg(feature = "perf-alloc")]
fn record_allocation(bytes: u64, contributes: bool) {
    ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
    ALLOCATED_BYTES.fetch_add(bytes, Ordering::Relaxed);
    add_live_bytes(bytes, contributes);
}

#[cfg(feature = "perf-alloc")]
fn add_live_bytes(bytes: u64, contributes: bool) {
    let live = LIVE_BYTES.fetch_add(bytes, Ordering::Relaxed) + bytes;
    let mut peak = PEAK_LIVE_BYTES.load(Ordering::Relaxed);
    while live > peak {
        match PEAK_LIVE_BYTES.compare_exchange_weak(
            peak,
            live,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(observed) => peak = observed,
        }
    }
    publish_measured_peak(contributes, &MEASURED_PEAK_LIVE_BYTES, live);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PerformanceScenario {
    FourBotStress,
    MapCycle100,
    Soak10Minutes,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AssetPreloadStatus {
    Pending,
    Ready,
    Failed,
}

fn combine_asset_preload_statuses(
    statuses: impl IntoIterator<Item = AssetPreloadStatus>,
) -> AssetPreloadStatus {
    let mut all_ready = true;
    let mut observed = false;
    for status in statuses {
        observed = true;
        match status {
            AssetPreloadStatus::Failed => return AssetPreloadStatus::Failed,
            AssetPreloadStatus::Pending => all_ready = false,
            AssetPreloadStatus::Ready => {}
        }
    }
    if observed && all_ready {
        AssetPreloadStatus::Ready
    } else {
        AssetPreloadStatus::Pending
    }
}

fn continuous_update_mode_valid(settings: &WinitSettings) -> bool {
    matches!(settings.focused_mode, UpdateMode::Continuous)
        && matches!(settings.unfocused_mode, UpdateMode::Continuous)
}

impl PerformanceScenario {
    fn parse(value: &str) -> Option<Self> {
        let normalized = value
            .chars()
            .filter(|character| character.is_ascii_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect::<String>();
        match normalized.as_str() {
            "fourbotstress" => Some(Self::FourBotStress),
            "mapcycle100" => Some(Self::MapCycle100),
            "soak10minutes" | "soak" => Some(Self::Soak10Minutes),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::FourBotStress => "FourBotStress",
            Self::MapCycle100 => "MapCycle100",
            Self::Soak10Minutes => "Soak10Minutes",
        }
    }

    fn default_measurement_seconds(self) -> f64 {
        match self {
            Self::FourBotStress | Self::MapCycle100 => DEFAULT_STRESS_SECONDS,
            Self::Soak10Minutes => DEFAULT_SOAK_SECONDS,
        }
    }

    fn uses_four_bots(self) -> bool {
        matches!(self, Self::FourBotStress | Self::Soak10Minutes)
    }
}

pub(crate) fn active_scenario_requested_from_environment() -> bool {
    std::env::var("AFC_PERF_SCENARIO")
        .ok()
        .and_then(|value| PerformanceScenario::parse(&value))
        .is_some()
}

pub(crate) fn uncapped_present_mode_requested_from_environment() -> bool {
    active_scenario_requested_from_environment() && environment_flag("AFC_PERF_UNCAPPED")
}

#[derive(Debug, Clone)]
struct ScenarioConfig {
    scenario: PerformanceScenario,
    warmup_seconds: f64,
    measurement_seconds: f64,
    seed: u64,
    run_id: String,
    uncapped_present_mode: bool,
}

impl ScenarioConfig {
    fn from_environment() -> Option<Self> {
        let requested = std::env::var("AFC_PERF_SCENARIO").ok()?;
        let Some(scenario) = PerformanceScenario::parse(&requested) else {
            warn!(
                "Unknown AFC_PERF_SCENARIO={requested:?}; expected FourBotStress, MapCycle100, or Soak10Minutes"
            );
            return None;
        };
        let warmup_seconds = environment_f64("AFC_PERF_WARMUP_SECONDS")
            .unwrap_or(DEFAULT_WARMUP_SECONDS)
            .max(0.0);
        let measurement_seconds = environment_f64("AFC_PERF_MEASUREMENT_SECONDS")
            .unwrap_or_else(|| scenario.default_measurement_seconds())
            .max(0.1);
        let seed = std::env::var("AFC_PERF_SEED")
            .ok()
            .and_then(|value| parse_seed(&value))
            .unwrap_or(DEFAULT_REPLAY_SEED);
        let run_id = std::env::var("AFC_PERF_RUN_ID").unwrap_or_else(|_| "unlabeled".to_string());
        let uncapped_present_mode = environment_flag("AFC_PERF_UNCAPPED");
        Some(Self {
            scenario,
            warmup_seconds,
            measurement_seconds,
            seed,
            run_id,
            uncapped_present_mode,
        })
    }
}

#[derive(Resource)]
struct MapCycleAssetPreload {
    handles: Vec<Handle<LoadedUntypedAsset>>,
}

impl MapCycleAssetPreload {
    fn request(asset_server: &AssetServer) -> io::Result<Self> {
        let paths = discover_map_cycle_asset_paths()?;
        if paths.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "map-cycle preload catalog is empty",
            ));
        }
        Ok(Self {
            handles: paths
                .into_iter()
                .map(|path| asset_server.load_untyped(path))
                .collect(),
        })
    }

    fn asset_status(
        asset_server: &AssetServer,
        handle: &Handle<LoadedUntypedAsset>,
    ) -> AssetPreloadStatus {
        let Some((load, dependencies, recursive_dependencies)) =
            asset_server.get_load_states(handle.id())
        else {
            return AssetPreloadStatus::Pending;
        };
        if load.is_failed() || dependencies.is_failed() || recursive_dependencies.is_failed() {
            AssetPreloadStatus::Failed
        } else if asset_server.is_loaded_with_dependencies(handle.id()) {
            AssetPreloadStatus::Ready
        } else {
            AssetPreloadStatus::Pending
        }
    }

    fn status(&self, asset_server: &AssetServer) -> AssetPreloadStatus {
        combine_asset_preload_statuses(
            self.handles
                .iter()
                .map(|handle| Self::asset_status(asset_server, handle)),
        )
    }
}

fn map_cycle_asset_root() -> PathBuf {
    std::env::var_os("BEVY_ASSET_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")))
        .join("assets")
}

fn discover_map_cycle_asset_paths() -> io::Result<Vec<String>> {
    let asset_root = map_cycle_asset_root();
    discover_map_cycle_asset_paths_from_root(&asset_root)
}

fn discover_map_cycle_asset_paths_from_root(asset_root: &Path) -> io::Result<Vec<String>> {
    let mut directories = MAP_CYCLE_PRELOAD_FOLDERS
        .iter()
        .map(|folder| asset_root.join(folder))
        .collect::<Vec<_>>();
    let mut paths = Vec::new();
    while let Some(directory) = directories.pop() {
        for entry in std::fs::read_dir(&directory)? {
            let path = entry?.path();
            if path.is_dir() {
                directories.push(path);
                continue;
            }
            let supported = path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    extension.eq_ignore_ascii_case("glb")
                        || extension.eq_ignore_ascii_case("png")
                        || extension.eq_ignore_ascii_case("ogg")
                        || extension.eq_ignore_ascii_case("mp3")
                        || extension.eq_ignore_ascii_case("wav")
                });
            if !supported {
                continue;
            }
            let relative = path.strip_prefix(asset_root).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("asset path escaped preload root: {error}"),
                )
            })?;
            paths.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
    paths.sort_unstable();
    paths.dedup();
    Ok(paths)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CanonicalCaptureEligibility {
    warmup: bool,
    duration: bool,
    seed: bool,
    present_mode: bool,
}

impl CanonicalCaptureEligibility {
    fn from_config(config: &ScenarioConfig) -> Self {
        Self {
            warmup: config.warmup_seconds == DEFAULT_WARMUP_SECONDS,
            duration: config.measurement_seconds == config.scenario.default_measurement_seconds(),
            seed: config.seed == DEFAULT_REPLAY_SEED,
            present_mode: !config.uncapped_present_mode,
        }
    }

    fn eligible(self) -> bool {
        self.warmup && self.duration && self.seed && self.present_mode
    }
}

fn present_mode_policy(uncapped: bool) -> &'static str {
    if uncapped { "AutoNoVsync" } else { "AutoVsync" }
}

fn expected_present_mode(uncapped: bool) -> PresentMode {
    if uncapped {
        PresentMode::AutoNoVsync
    } else {
        PresentMode::AutoVsync
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FixedCombatFixtureArena {
    index: usize,
    name: &'static str,
    authored_items: usize,
    authored_hazards: usize,
    public_hazard_markers: usize,
}

fn fixed_combat_fixture_arena(scenario: PerformanceScenario) -> Option<FixedCombatFixtureArena> {
    if !scenario.uses_four_bots() {
        return None;
    }
    let arena = &arena_definitions()[BUMPER_ALLEY_ARENA_INDEX];
    Some(FixedCombatFixtureArena {
        index: BUMPER_ALLEY_ARENA_INDEX,
        name: arena.name,
        authored_items: arena.item_anchors.len(),
        authored_hazards: arena.hazards.len(),
        public_hazard_markers: expected_hazard_marker_count(BUMPER_ALLEY_ARENA_INDEX),
    })
}

fn environment_f64(name: &str) -> Option<f64> {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite())
}

fn parse_seed(value: &str) -> Option<u64> {
    let trimmed = value.trim();
    if let Some(hex) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        u64::from_str_radix(hex, 16).ok()
    } else {
        trimmed.parse().ok()
    }
}

/// Cross-world request/acknowledgement for an acquired window surface texture
/// that Bevy consumed through `SurfaceTexture::present`.
///
/// This proves the present invocation that schedules an acquired texture for
/// display. It deliberately does not claim GPU execution, scanout, or
/// completion: wgpu performs those asynchronously.
#[derive(Resource, Clone)]
struct SurfacePresentFence {
    /// A normal main-world field. Extraction observes this value through Bevy
    /// change detection; only the renderer acknowledgement is shared.
    requested_epoch: u64,
    observed_epoch: std::sync::Arc<AtomicU64>,
    trace_enabled: bool,
}

impl Default for SurfacePresentFence {
    fn default() -> Self {
        Self {
            requested_epoch: 0,
            observed_epoch: std::sync::Arc::new(AtomicU64::new(0)),
            trace_enabled: false,
        }
    }
}

impl SurfacePresentFence {
    fn request(&mut self) -> u64 {
        self.requested_epoch = self
            .requested_epoch
            .checked_add(1)
            .expect("present fence epoch exhausted");
        if self.trace_enabled {
            info!(
                "AFC_PERF_PRESENT_TRACE main_request epoch={}",
                self.requested_epoch
            );
        }
        self.requested_epoch
    }

    fn has_observed(&self, epoch: u64) -> bool {
        self.observed_epoch.load(Ordering::Acquire) >= epoch
    }
}

impl ExtractResource for SurfacePresentFence {
    type Source = Self;

    fn extract_resource(source: &Self::Source) -> Self {
        source.clone()
    }
}

/// Main-world fixture facts extracted beside a requested present epoch.
///
/// This resource is rebuilt in `Last`, after gameplay mutation and immediately
/// before render extraction. The render-world probe will not arm from an epoch
/// alone: the exact scene generation and all fixture counts must match here.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PresentFixtureEvidence {
    epoch: u64,
    scene_identity: Option<ArenaRenderIdentity>,
    expected_fighters: usize,
    observed_fighters: usize,
    expected_combatant_bots: usize,
    observed_combatant_bots: usize,
    expected_items: usize,
    observed_items: usize,
    expected_hazard_markers: usize,
    observed_hazard_markers: usize,
    scene_root_count: usize,
    ready_scene_instance_count: usize,
    scene_root_readiness_non_vacuous: bool,
    scene_instances_ready: bool,
    valid: bool,
}

impl PresentFixtureEvidence {
    fn counts_are_exact(self) -> bool {
        self.expected_fighters == self.observed_fighters
            && self.expected_combatant_bots == self.observed_combatant_bots
            && self.expected_items == self.observed_items
            && self.expected_hazard_markers == self.observed_hazard_markers
    }

    fn proves(self, epoch: u64, scene_identity: Option<ArenaRenderIdentity>) -> bool {
        epoch != 0
            && self.epoch == epoch
            && self.scene_identity == scene_identity
            && scene_identity.is_some()
            && self.scene_root_readiness_non_vacuous
            && self.scene_root_count > 0
            && self.ready_scene_instance_count == self.scene_root_count
            && self.scene_instances_ready
            && self.valid
            && self.counts_are_exact()
    }

    fn allows_render_arm(self, epoch: u64) -> bool {
        self.proves(epoch, self.scene_identity)
    }
}

impl ExtractResource for PresentFixtureEvidence {
    type Source = Self;

    fn extract_resource(source: &Self::Source) -> Self {
        *source
    }
}

#[derive(Resource, Default)]
struct SurfacePresentProbe {
    epoch: u64,
    window: Option<Entity>,
    view: Option<Entity>,
    view_id: Option<TextureViewId>,
    trace_epoch: u64,
    trace_arm_samples_remaining: u8,
    trace_post_samples_remaining: u8,
}

impl SurfacePresentProbe {
    fn take_arm_trace_sample(&mut self, enabled: bool, epoch: u64) -> bool {
        if !enabled || epoch == 0 {
            return false;
        }
        if self.trace_epoch != epoch {
            self.trace_epoch = epoch;
            self.trace_arm_samples_remaining = 4;
            self.trace_post_samples_remaining = 4;
        }
        if self.trace_arm_samples_remaining == 0 {
            return false;
        }
        self.trace_arm_samples_remaining -= 1;
        true
    }

    fn take_post_trace_sample(&mut self, enabled: bool, epoch: u64) -> bool {
        if !enabled || epoch == 0 || self.trace_epoch != epoch {
            return false;
        }
        if self.trace_post_samples_remaining == 0 {
            return false;
        }
        self.trace_post_samples_remaining -= 1;
        true
    }
}

/// Arms evidence only when Bevy has acquired a swapchain texture and the exact
/// output view for a camera targeting that window.
///
/// `ViewTarget::needs_present` cannot be used here: Bevy sets that flag while
/// the render graph writes the output attachment inside `render_system`.
fn arm_surface_present_probe(
    fence: Res<SurfacePresentFence>,
    fixture: Res<PresentFixtureEvidence>,
    windows: Res<ExtractedWindows>,
    views: Query<(Entity, &ViewTarget, &ExtractedCamera)>,
    mut probe: ResMut<SurfacePresentProbe>,
) {
    probe.epoch = 0;
    probe.window = None;
    probe.view = None;
    probe.view_id = None;
    let requested = fence.requested_epoch;
    let trace = probe.take_arm_trace_sample(fence.trace_enabled, requested);
    let observed = requested != 0 && fence.has_observed(requested);
    let fixture_allows_arm = fixture.allows_render_arm(requested);
    if trace {
        info!(
            "AFC_PERF_PRESENT_TRACE render_arm_input epoch={} observed={} fixture_allows_arm={} fixture={:?}",
            requested, observed, fixture_allows_arm, *fixture
        );
    }
    if requested == 0 || observed || !fixture_allows_arm {
        return;
    }

    let Some(primary) = windows.primary else {
        if trace {
            info!(
                "AFC_PERF_PRESENT_TRACE render_arm_result epoch={} result=missing_primary_window extracted_windows={}",
                requested,
                windows.len()
            );
        }
        return;
    };
    let Some(window) = windows.get(&primary) else {
        if trace {
            info!(
                "AFC_PERF_PRESENT_TRACE render_arm_result epoch={} result=missing_primary_entry primary={:?} extracted_windows={}",
                requested,
                primary,
                windows.len()
            );
        }
        return;
    };
    let Some(view_id) = window
        .swap_chain_texture_view
        .as_ref()
        .map(|view| view.id())
    else {
        if trace {
            info!(
                "AFC_PERF_PRESENT_TRACE render_arm_result epoch={} result=missing_surface_view primary={:?} surface_texture={} initial_present={}",
                requested,
                primary,
                window.swap_chain_texture.is_some(),
                window.needs_initial_present
            );
        }
        return;
    };
    let matching_view = views.iter().find_map(|(entity, view_target, camera)| {
        matches!(
            camera.target,
            Some(NormalizedRenderTarget::Window(reference))
                if reference.entity() == window.entity
        )
        .then(|| (entity, view_target.out_texture().id()))
        .filter(|(_, target_view_id)| *target_view_id == view_id)
        .map(|(entity, _)| entity)
    });
    let Some(view) = matching_view else {
        if trace {
            info!(
                "AFC_PERF_PRESENT_TRACE render_arm_result epoch={} result=no_exact_camera_output primary={:?} surface_view_id={:?} surface_texture={} initial_present={} render_views={}",
                requested,
                primary,
                view_id,
                window.swap_chain_texture.is_some(),
                window.needs_initial_present,
                views.iter().count()
            );
            for (entity, view_target, camera) in &views {
                info!(
                    "AFC_PERF_PRESENT_TRACE render_view epoch={} entity={:?} target={:?} output_view_id={:?} needs_present_before={}",
                    requested,
                    entity,
                    camera.target,
                    view_target.out_texture().id(),
                    view_target.needs_present()
                );
            }
        }
        return;
    };
    if window.swap_chain_texture.is_some() {
        probe.epoch = requested;
        probe.window = Some(window.entity);
        probe.view = Some(view);
        probe.view_id = Some(view_id);
        if trace {
            info!(
                "AFC_PERF_PRESENT_TRACE render_arm_result epoch={} result=armed primary={:?} view={:?} view_id={:?} initial_present={}",
                requested, primary, view, view_id, window.needs_initial_present
            );
        }
    } else if trace {
        info!(
            "AFC_PERF_PRESENT_TRACE render_arm_result epoch={} result=missing_surface_texture primary={:?} view={:?} view_id={:?} initial_present={}",
            requested, primary, view, view_id, window.needs_initial_present
        );
    }
}

/// Runs immediately after Bevy's `render_system`. The exact acquired texture
/// armed above can become `None` only because that system consumed it through
/// `ExtractedWindow::present`. Requiring the same output attachment to report
/// that it was written proves the renderer's public present condition too; a
/// render failure panics before this system runs.
fn acknowledge_surface_present_probe(
    fence: Res<SurfacePresentFence>,
    windows: Res<ExtractedWindows>,
    views: Query<(&ViewTarget, &ExtractedCamera)>,
    mut probe: ResMut<SurfacePresentProbe>,
) {
    let armed_epoch = probe.epoch;
    let trace = probe.take_post_trace_sample(fence.trace_enabled, armed_epoch);
    let Some(window) = probe.window.take() else {
        return;
    };
    let Some(view) = probe.view.take() else {
        probe.epoch = 0;
        probe.view_id = None;
        return;
    };
    let Some(view_id) = probe.view_id.take() else {
        probe.epoch = 0;
        return;
    };
    let epoch = std::mem::take(&mut probe.epoch);
    let target_written = views.get(view).is_ok_and(|(view_target, camera)| {
        matches!(
            camera.target,
            Some(NormalizedRenderTarget::Window(reference))
                if reference.entity() == window
        ) && view_target.out_texture().id() == view_id
            && view_target.needs_present()
    });
    let exact_surface_consumed = windows.get(&window).is_some_and(|window| {
        window.swap_chain_texture.is_none()
            && window
                .swap_chain_texture_view
                .as_ref()
                .is_some_and(|view| view.id() == view_id)
    });
    if trace {
        info!(
            "AFC_PERF_PRESENT_TRACE render_post epoch={} window={:?} view={:?} view_id={:?} target_written={} exact_surface_consumed={} observed={}",
            epoch,
            window,
            view,
            view_id,
            target_written,
            exact_surface_consumed,
            present_invocation_observed(epoch, target_written, exact_surface_consumed)
        );
    }
    if present_invocation_observed(epoch, target_written, exact_surface_consumed) {
        fence.observed_epoch.fetch_max(epoch, Ordering::AcqRel);
    }
}

fn present_invocation_observed(
    epoch: u64,
    target_written: bool,
    exact_surface_consumed: bool,
) -> bool {
    epoch != 0 && target_written && exact_surface_consumed
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MeasurementPhase {
    Configure,
    AwaitSimulationReady,
    AwaitInitialPresent,
    Warmup,
    Measure,
    AwaitFinalPresent,
    Complete,
    Failed,
}

#[derive(Clone, Copy, Debug, Default)]
struct OwnerActivity {
    actions: usize,
    hits: usize,
    guards: usize,
    items: usize,
    abilities: usize,
}

impl OwnerActivity {
    fn total(self) -> usize {
        self.actions + self.hits + self.guards + self.items + self.abilities
    }
}

/// Fixed, owner-indexed snapshot of actual live combat work. The fixture never
/// derives this from fighter roots: it counts each dynamic system that creates
/// hit/contact work (attack hitboxes and ability entities) by stable owner.
#[derive(Clone, Copy, Debug, Default)]
struct OwnerWorkload {
    hitboxes: usize,
    specials: usize,
    bee_skills: usize,
    chick_skills: usize,
    penguin_skills: usize,
    penguin_surfaces: usize,
}

impl OwnerWorkload {
    fn total(self) -> usize {
        self.hitboxes
            + self.specials
            + self.bee_skills
            + self.chick_skills
            + self.penguin_skills
            + self.penguin_surfaces
    }

    fn max_assign(&mut self, next: Self) {
        self.hitboxes = self.hitboxes.max(next.hitboxes);
        self.specials = self.specials.max(next.specials);
        self.bee_skills = self.bee_skills.max(next.bee_skills);
        self.chick_skills = self.chick_skills.max(next.chick_skills);
        self.penguin_skills = self.penguin_skills.max(next.penguin_skills);
        self.penguin_surfaces = self.penguin_surfaces.max(next.penguin_surfaces);
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ResourceCounts {
    entities: usize,
    meshes: usize,
    materials: usize,
    images: usize,
}

#[derive(Clone, Copy, Debug, Default)]
struct ResourceGrowthSample {
    elapsed_seconds: f64,
    resources: ResourceCounts,
    resident_gib: f64,
    live_bytes: u64,
    stale_owner_entities: usize,
}

#[derive(Clone, Copy, Debug, Default)]
struct ProcessMemoryObservation {
    elapsed_seconds: f64,
    resident_gib: f64,
}

#[derive(Clone, Copy, Debug, Default)]
struct MapCycleCheckpoint {
    cycle: usize,
    present_epoch: u64,
    elapsed_seconds: f64,
    resources: ResourceCounts,
    resident_gib: Option<f64>,
    live_bytes: u64,
}

#[derive(Clone, Copy, Debug, Default)]
struct AlignedCycleGrowthAnalysis {
    checkpoint_count: usize,
    interval_count: usize,
    span_seconds: f64,
    resource_counts_valid: bool,
    resident_range_mib: f64,
    resident_slope_mib_per_minute: f64,
    resident_plateau: bool,
    live_range_mib: f64,
    live_slope_mib_per_minute: f64,
    live_plateau: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct FrameClassificationCounts {
    steady: usize,
    transition: usize,
    finalization: usize,
}

impl FrameClassificationCounts {
    fn total(self) -> usize {
        self.steady + self.transition + self.finalization
    }

    fn valid_for(self, frame_samples: usize) -> bool {
        self.total() == frame_samples
    }
}

#[derive(Clone, Copy, Debug)]
struct MonotonicTracker {
    initialized: bool,
    first: f64,
    last: f64,
    non_decreasing: bool,
}

impl Default for MonotonicTracker {
    fn default() -> Self {
        Self {
            initialized: false,
            first: 0.0,
            last: 0.0,
            non_decreasing: true,
        }
    }
}

impl MonotonicTracker {
    fn observe(&mut self, value: f64) {
        if !self.initialized {
            self.initialized = true;
            self.first = value;
            self.last = value;
            return;
        }
        if value < self.last {
            self.non_decreasing = false;
        }
        self.last = value;
    }

    fn reports_growth(self) -> bool {
        self.initialized && self.non_decreasing && self.last > self.first
    }
}

#[derive(Clone, Copy, Debug)]
struct BoundedResourceGrowth {
    history: [Option<ResourceGrowthSample>; RESOURCE_GROWTH_HISTORY_SAMPLES],
    history_len: usize,
    tail: [Option<ResourceGrowthSample>; RESOURCE_GROWTH_TAIL_SAMPLES],
    tail_len: usize,
    total_observed: usize,
    thinning_count: usize,
    monotonic: [MonotonicTracker; 6],
}

impl Default for BoundedResourceGrowth {
    fn default() -> Self {
        Self {
            history: [None; RESOURCE_GROWTH_HISTORY_SAMPLES],
            history_len: 0,
            tail: [None; RESOURCE_GROWTH_TAIL_SAMPLES],
            tail_len: 0,
            total_observed: 0,
            thinning_count: 0,
            monotonic: [MonotonicTracker::default(); 6],
        }
    }
}

impl BoundedResourceGrowth {
    fn clear(&mut self) {
        self.history.fill(None);
        self.history_len = 0;
        self.tail.fill(None);
        self.tail_len = 0;
        self.total_observed = 0;
        self.thinning_count = 0;
        self.monotonic = [MonotonicTracker::default(); 6];
    }

    fn push(&mut self, sample: ResourceGrowthSample) {
        for (tracker, value) in self.monotonic.iter_mut().zip([
            sample.resources.entities as f64,
            sample.resources.meshes as f64,
            sample.resources.materials as f64,
            sample.resources.images as f64,
            sample.resident_gib,
            sample.live_bytes as f64,
        ]) {
            tracker.observe(value);
        }
        self.total_observed = self.total_observed.saturating_add(1);
        if self.last().is_some_and(|last| {
            (last.elapsed_seconds - sample.elapsed_seconds).abs() < f64::EPSILON
        }) {
            if self.tail_len > 0 {
                self.tail[self.tail_len - 1] = Some(sample);
            } else if self.history_len > 0 {
                self.history[self.history_len - 1] = Some(sample);
            }
            return;
        }
        if self.tail_len == self.tail.len() {
            let evicted = self.tail[0].expect("a full resource tail contains samples");
            self.push_history(evicted);
            self.tail.copy_within(1..self.tail_len, 0);
            self.tail_len -= 1;
        }
        self.tail[self.tail_len] = Some(sample);
        self.tail_len += 1;
    }

    fn push_history(&mut self, sample: ResourceGrowthSample) {
        if self.history_len < self.history.len() {
            self.history[self.history_len] = Some(sample);
            self.history_len += 1;
            return;
        }

        // Preserve the exact start and newest historical edge. Remove the
        // interior point in the densest local time span, then append the
        // evicted tail sample. Repeating this bounded thinning retains broad
        // whole-run coverage instead of biasing all history toward one edge.
        let mut remove_index = 1;
        let mut smallest_span = f64::INFINITY;
        for index in 1..self.history_len {
            let previous = self.history[index - 1]
                .expect("full resource history contains a predecessor")
                .elapsed_seconds;
            let next = if index + 1 < self.history_len {
                self.history[index + 1]
                    .expect("full resource history contains a successor")
                    .elapsed_seconds
            } else {
                sample.elapsed_seconds
            };
            let span = next - previous;
            if span < smallest_span {
                smallest_span = span;
                remove_index = index;
            }
        }
        self.history
            .copy_within(remove_index + 1..self.history_len, remove_index);
        self.history[self.history_len - 1] = Some(sample);
        self.thinning_count = self.thinning_count.saturating_add(1);
    }

    fn len(&self) -> usize {
        self.history_len + self.tail_len
    }

    fn iter(&self) -> impl DoubleEndedIterator<Item = &ResourceGrowthSample> {
        self.history[..self.history_len]
            .iter()
            .flatten()
            .chain(self.tail[..self.tail_len].iter().flatten())
    }

    fn first(&self) -> Option<ResourceGrowthSample> {
        self.iter().next().copied()
    }

    fn last(&self) -> Option<ResourceGrowthSample> {
        self.iter().next_back().copied()
    }

    fn monotonic_growth(&self, index: usize) -> bool {
        self.monotonic[index].reports_growth()
    }
}

#[derive(Resource)]
struct ScenarioRun {
    config: ScenarioConfig,
    phase: MeasurementPhase,
    measurement_started: bool,
    warmup_elapsed: f64,
    measurement_elapsed: f64,
    frame_ms: Vec<f64>,
    cpu_frame_ms: Vec<f64>,
    frame_cpu_started: Option<Instant>,
    frame_classification: FrameClassificationCounts,
    classify_current_frame_transition: bool,
    process_cpu_percent: Vec<f64>,
    process_memory_gib: Vec<f64>,
    process_memory_observations: Vec<ProcessMemoryObservation>,
    previous_process_cpu_sample: Option<(f64, f64)>,
    next_system_sample_seconds: f64,
    last_resident_gib: f64,
    resource_growth: BoundedResourceGrowth,
    next_resource_sample_seconds: f64,
    resource_sample_interval_seconds: f64,
    resource_start: Option<ResourceCounts>,
    map_first_cycle_resources: Option<ResourceCounts>,
    entity_peak: usize,
    entity_end: usize,
    mesh_peak: usize,
    mesh_end: usize,
    material_peak: usize,
    material_end: usize,
    image_peak: usize,
    image_end: usize,
    allocation_count_start: u64,
    allocated_bytes_start: u64,
    live_bytes_start: u64,
    map_switches: usize,
    switch_ms: Vec<f64>,
    map_warm_precycle_switches: usize,
    map_warm_precycle_present_ack_count: usize,
    map_warm_precycle_valid: bool,
    map_measured_present_ack_count: usize,
    map_cycle_checkpoints: Vec<MapCycleCheckpoint>,
    map_checkpoint_pending: Option<(usize, u64)>,
    pending_switch_started: Option<Instant>,
    pending_present_epoch: Option<u64>,
    pending_scene_identity: Option<ArenaRenderIdentity>,
    initial_present_ack_count: usize,
    final_present_ack_count: usize,
    present_ack_count: usize,
    final_present_requested_epoch: u64,
    final_present_observed_epoch: u64,
    phase_started: Instant,
    activity: [OwnerActivity; 4],
    owner_end: [OwnerWorkload; 4],
    owner_peak: [OwnerWorkload; 4],
    stale_owner_entities_peak: usize,
    stale_owner_entities_end: usize,
    fixture_expected_fighters: usize,
    fixture_observed_fighters: usize,
    fixture_expected_combatant_bots: usize,
    fixture_observed_combatant_bots: usize,
    fixture_expected_items: usize,
    fixture_observed_items: usize,
    fixture_expected_hazard_markers: usize,
    fixture_observed_hazard_markers: usize,
    fixture_counts_valid: bool,
    scene_root_count: usize,
    ready_scene_instance_count: usize,
    scene_root_readiness_non_vacuous: bool,
    scene_instances_ready: bool,
    continuous_update_mode_valid: bool,
    present_mode_policy_valid: bool,
    bots_promoted_from_canonical_fixture: bool,
    map_cycle_preload_ready: bool,
    map_cycle_preload_asset_count: usize,
    readiness_valid: bool,
    fence_valid: bool,
    failure: Option<&'static str>,
    last_activity_tick: Option<crate::simulation::SimTick>,
    journal_gap_ticks: u64,
    event_overflow_start: u32,
    event_overflow_end: u32,
    pending_exit: Option<AppExit>,
}

fn pretouched_f64_buffer(capacity: usize) -> Vec<f64> {
    let mut buffer = vec![1.0; capacity];
    std::hint::black_box(buffer.as_slice());
    buffer.clear();
    buffer
}

impl ScenarioRun {
    fn new(config: ScenarioConfig) -> Self {
        let expected_frames = (config.measurement_seconds * 600.0).ceil() as usize;
        let expected_system_samples = config.measurement_seconds.ceil() as usize + 2;
        let map_cycle_preload_ready = config.scenario != PerformanceScenario::MapCycle100;
        let map_warm_precycle_valid = config.scenario != PerformanceScenario::MapCycle100;
        let resource_sample_interval_seconds =
            (config.measurement_seconds / (MAX_RESOURCE_GROWTH_SAMPLES - 1) as f64).max(0.1);
        Self {
            config,
            phase: MeasurementPhase::Configure,
            measurement_started: false,
            warmup_elapsed: 0.0,
            measurement_elapsed: 0.0,
            frame_ms: pretouched_f64_buffer(expected_frames),
            cpu_frame_ms: pretouched_f64_buffer(expected_frames),
            frame_cpu_started: None,
            frame_classification: FrameClassificationCounts::default(),
            classify_current_frame_transition: false,
            process_cpu_percent: Vec::with_capacity(expected_system_samples),
            process_memory_gib: Vec::with_capacity(expected_system_samples),
            process_memory_observations: Vec::with_capacity(expected_system_samples),
            previous_process_cpu_sample: None,
            next_system_sample_seconds: 0.0,
            last_resident_gib: 0.0,
            resource_growth: BoundedResourceGrowth::default(),
            next_resource_sample_seconds: 0.0,
            resource_sample_interval_seconds,
            resource_start: None,
            map_first_cycle_resources: None,
            entity_peak: 0,
            entity_end: 0,
            mesh_peak: 0,
            mesh_end: 0,
            material_peak: 0,
            material_end: 0,
            image_peak: 0,
            image_end: 0,
            allocation_count_start: 0,
            allocated_bytes_start: 0,
            live_bytes_start: 0,
            map_switches: 0,
            switch_ms: Vec::with_capacity(MAP_SWITCH_COUNT),
            map_warm_precycle_switches: 0,
            map_warm_precycle_present_ack_count: 0,
            map_warm_precycle_valid,
            map_measured_present_ack_count: 0,
            map_cycle_checkpoints: Vec::with_capacity(MAP_ALIGNED_CHECKPOINT_COUNT),
            map_checkpoint_pending: None,
            pending_switch_started: None,
            pending_present_epoch: None,
            pending_scene_identity: None,
            initial_present_ack_count: 0,
            final_present_ack_count: 0,
            present_ack_count: 0,
            final_present_requested_epoch: 0,
            final_present_observed_epoch: 0,
            phase_started: Instant::now(),
            activity: [OwnerActivity::default(); 4],
            owner_end: [OwnerWorkload::default(); 4],
            owner_peak: [OwnerWorkload::default(); 4],
            stale_owner_entities_peak: 0,
            stale_owner_entities_end: 0,
            fixture_expected_fighters: 0,
            fixture_observed_fighters: 0,
            fixture_expected_combatant_bots: 0,
            fixture_observed_combatant_bots: 0,
            fixture_expected_items: 0,
            fixture_observed_items: 0,
            fixture_expected_hazard_markers: 0,
            fixture_observed_hazard_markers: 0,
            fixture_counts_valid: false,
            scene_root_count: 0,
            ready_scene_instance_count: 0,
            scene_root_readiness_non_vacuous: false,
            scene_instances_ready: false,
            continuous_update_mode_valid: false,
            present_mode_policy_valid: false,
            bots_promoted_from_canonical_fixture: false,
            map_cycle_preload_ready,
            map_cycle_preload_asset_count: 0,
            readiness_valid: false,
            fence_valid: false,
            failure: None,
            last_activity_tick: None,
            journal_gap_ticks: 0,
            event_overflow_start: 0,
            event_overflow_end: 0,
            pending_exit: None,
        }
    }

    fn request_exit(&mut self, status: AppExit) {
        assert!(
            self.pending_exit.is_none(),
            "performance scenario requested more than one terminal exit"
        );
        self.pending_exit = Some(status);
    }

    fn begin_measurement(
        &mut self,
        event_overflow_start: u32,
        warmup_boundary_tick: Option<crate::simulation::SimTick>,
    ) {
        self.measurement_started = true;
        self.phase = MeasurementPhase::Measure;
        self.measurement_elapsed = 0.0;
        self.next_system_sample_seconds = 0.0;
        self.last_resident_gib = 0.0;
        self.process_cpu_percent.clear();
        self.process_memory_gib.clear();
        self.process_memory_observations.clear();
        self.previous_process_cpu_sample = None;
        self.resource_growth.clear();
        self.next_resource_sample_seconds = 0.0;
        self.resource_start = None;
        self.map_first_cycle_resources = None;
        self.frame_ms.clear();
        self.cpu_frame_ms.clear();
        self.frame_classification = FrameClassificationCounts::default();
        self.classify_current_frame_transition = false;
        self.map_switches = 0;
        self.switch_ms.clear();
        self.map_measured_present_ack_count = 0;
        self.map_cycle_checkpoints.clear();
        self.map_checkpoint_pending = (self.config.scenario == PerformanceScenario::MapCycle100)
            .then_some((0, self.final_present_observed_epoch));
        let allocations = begin_allocation_measurement();
        self.allocation_count_start = allocations.allocation_count;
        self.allocated_bytes_start = allocations.allocated_bytes;
        self.live_bytes_start = allocations.live_bytes;
        self.activity = [OwnerActivity::default(); 4];
        self.owner_end = [OwnerWorkload::default(); 4];
        self.owner_peak = [OwnerWorkload::default(); 4];
        self.stale_owner_entities_peak = 0;
        self.stale_owner_entities_end = 0;
        // Activity observed during warmup is excluded. Seeding the boundary
        // means the first measured render frame scans every fixed tick committed
        // after warmup, even if several simulation steps occurred in that frame.
        self.last_activity_tick = warmup_boundary_tick;
        self.journal_gap_ticks = 0;
        self.event_overflow_start = event_overflow_start;
        self.event_overflow_end = event_overflow_start;
        info!(
            "AFC_PERF_MEASUREMENT_BEGIN scenario={} run_id={} duration_seconds={:.3}",
            self.config.scenario.label(),
            self.config.run_id,
            self.config.measurement_seconds
        );
    }

    fn apply_present_fixture_evidence(&mut self, evidence: PresentFixtureEvidence) {
        self.fixture_expected_fighters = evidence.expected_fighters;
        self.fixture_observed_fighters = evidence.observed_fighters;
        self.fixture_expected_combatant_bots = evidence.expected_combatant_bots;
        self.fixture_observed_combatant_bots = evidence.observed_combatant_bots;
        self.fixture_expected_items = evidence.expected_items;
        self.fixture_observed_items = evidence.observed_items;
        self.fixture_expected_hazard_markers = evidence.expected_hazard_markers;
        self.fixture_observed_hazard_markers = evidence.observed_hazard_markers;
        self.scene_root_count = evidence.scene_root_count;
        self.ready_scene_instance_count = evidence.ready_scene_instance_count;
        self.scene_root_readiness_non_vacuous = evidence.scene_root_readiness_non_vacuous;
        self.scene_instances_ready = evidence.scene_instances_ready;
        self.fixture_counts_valid = evidence.valid && evidence.counts_are_exact();
    }
}

/// Runtime settings for [`PerformancePlugin`].
#[derive(Resource, Debug, Clone)]
pub struct PerformanceConfig {
    /// Number of recent frame and entity-count samples retained for smoothing.
    pub history_length: usize,
    /// Interval between diagnostic snapshots written to the application log.
    pub log_interval: Duration,
}

impl Default for PerformanceConfig {
    fn default() -> Self {
        Self {
            history_length: 120,
            log_interval: DEFAULT_LOG_INTERVAL,
        }
    }
}

/// Registers diagnostics and, when requested, drives a seeded benchmark scenario.
pub struct PerformancePlugin {
    config: PerformanceConfig,
}

impl PerformancePlugin {
    pub fn new(config: PerformanceConfig) -> Self {
        Self { config }
    }
}

impl Default for PerformancePlugin {
    fn default() -> Self {
        Self::new(PerformanceConfig::default())
    }
}

impl Plugin for PerformancePlugin {
    fn build(&self, app: &mut App) {
        let history_length = self.config.history_length.max(1);
        let scenario_config = ScenarioConfig::from_environment();

        app.insert_resource(self.config.clone())
            .init_resource::<SurfacePresentFence>()
            .init_resource::<PresentFixtureEvidence>()
            .add_plugins(ExtractResourcePlugin::<SurfacePresentFence>::default())
            .add_plugins(ExtractResourcePlugin::<PresentFixtureEvidence>::default())
            .add_plugins((
                FrameTimeDiagnosticsPlugin::new(history_length),
                EntityCountDiagnosticsPlugin::new(history_length),
            ));
        if scenario_config.is_none() {
            let logged_diagnostics = [
                FrameTimeDiagnosticsPlugin::FPS,
                FrameTimeDiagnosticsPlugin::FRAME_TIME,
                EntityCountDiagnosticsPlugin::ENTITY_COUNT,
            ]
            .into_iter()
            .collect();
            app.add_plugins(LogDiagnosticsPlugin {
                wait_duration: self.config.log_interval,
                filter: Some(logged_diagnostics),
                ..default()
            });
        }
        app.world_mut()
            .resource_mut::<SurfacePresentFence>()
            .trace_enabled = environment_flag("AFC_PERF_PRESENT_TRACE");

        if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
            render_app
                .init_resource::<SurfacePresentProbe>()
                .add_systems(
                    Render,
                    arm_surface_present_probe
                        .in_set(RenderSystems::Render)
                        .before(render_system),
                )
                .add_systems(
                    Render,
                    acknowledge_surface_present_probe
                        .in_set(RenderSystems::Render)
                        .after(render_system)
                        .before(RenderSystems::Cleanup),
                );
        } else {
            error!("performance profiler requires Bevy's render sub-app for present fencing");
        }

        // Bevy's pass-level render diagnostics allocate while collecting paths and
        // Metal does not expose GPU timestamps through this plugin. Keep them out
        // of allocation/frame baselines unless a focused render capture asks for
        // them explicitly.
        if environment_flag("AFC_PERF_RENDER_DIAGNOSTICS") {
            app.add_plugins(RenderDiagnosticsPlugin);
        }

        if let Some(config) = scenario_config {
            info!(
                "AFC_PERF_REQUEST scenario={} run_id={} seed=0x{:016x} warmup_seconds={:.3} measurement_seconds={:.3}",
                config.scenario.label(),
                config.run_id,
                config.seed,
                config.warmup_seconds,
                config.measurement_seconds
            );
            // Bevy's normal game policy throttles an unfocused native window to
            // 60 Hz. Profiling must remain continuous when focus changes so the
            // strict wall-frame percentile includes workload, not an event-loop
            // sleep inserted by the desktop session.
            app.insert_resource(WinitSettings::continuous())
                .insert_resource(ScenarioRun::new(config))
                .add_systems(First, begin_frame_cpu_measurement)
                .add_systems(PreUpdate, dispatch_performance_exit)
                .add_systems(PreUpdate, configure_and_drive_scenario)
                .add_systems(
                    Last,
                    (
                        collect_scenario_metrics,
                        publish_present_fixture_evidence.after(collect_scenario_metrics),
                    ),
                );
        }
    }
}

/// Emits a terminal status one frame after `Last` records it.
#[cfg(not(target_os = "macos"))]
fn dispatch_performance_exit(mut run: ResMut<ScenarioRun>, mut exit: MessageWriter<AppExit>) {
    if let Some(status) = run.pending_exit.take() {
        exit.write(status);
    }
}

/// Closes native windows one update before publishing the terminal status.
///
/// Bevy 0.18.1 can deadlock on macOS when `AppExit` races the pipelined render
/// thread's window teardown. Despawning every Bevy window first makes Winit's
/// `Last` system retain and then drop the native wrappers on the main thread.
/// Keeping the original status pending until the next update also lets render
/// extraction remove the corresponding surfaces before application teardown.
#[cfg(target_os = "macos")]
fn dispatch_performance_exit(
    mut commands: Commands,
    windows: Query<Entity, With<Window>>,
    mut run: ResMut<ScenarioRun>,
    mut exit: MessageWriter<AppExit>,
) {
    if run.pending_exit.is_none() {
        return;
    }

    if !windows.is_empty() {
        for window in &windows {
            commands.entity(window).despawn();
        }
        return;
    }

    if let Some(status) = run.pending_exit.take() {
        exit.write(status);
    }
}

fn begin_frame_cpu_measurement(mut run: ResMut<ScenarioRun>) {
    run.frame_cpu_started = (run.phase == MeasurementPhase::Measure).then(Instant::now);
}

fn ready_arena_render_identity(
    scene: Option<&ArenaScene>,
    scene_markers: &Query<&ArenaSceneReadyMarker>,
    expected_index: usize,
) -> Option<ArenaRenderIdentity> {
    let scene = scene.filter(|scene| scene.index() == expected_index)?;
    let identity = (scene.index(), scene.generation());
    scene_markers
        .iter()
        .any(|marker| (marker.arena_index(), marker.generation()) == identity)
        .then_some(identity)
}

fn arena_scene_instance_readiness<'a>(
    scene_spawner: &SceneSpawner,
    instances: impl IntoIterator<Item = Option<&'a SceneInstance>>,
) -> (usize, usize, bool) {
    let mut root_count = 0;
    let mut ready_count = 0;
    for instance in instances {
        root_count += 1;
        if instance.is_some_and(|instance| scene_spawner.instance_is_ready(**instance)) {
            ready_count += 1;
        }
    }
    (
        root_count,
        ready_count,
        root_count > 0 && ready_count == root_count,
    )
}

fn publish_present_fixture_evidence(
    run: Res<ScenarioRun>,
    state: Res<MatchState>,
    scene: Option<Res<ArenaScene>>,
    scene_spawner: Res<SceneSpawner>,
    scene_markers: Query<&ArenaSceneReadyMarker>,
    arena_scene_instances: Query<Option<&SceneInstance>, (With<SceneRoot>, With<ArenaGeometry>)>,
    all_fighters: Query<&Fighter>,
    bot_brains: Query<(&Fighter, &Controller, &BotBrain)>,
    arena_items: Query<(), With<ArenaItem>>,
    arena_hazards: Query<(), With<ArenaHazardMarker>>,
    mut evidence: ResMut<PresentFixtureEvidence>,
) {
    *evidence = PresentFixtureEvidence::default();
    let Some(epoch) = run.pending_present_epoch else {
        return;
    };
    let expected_scene_identity = run.pending_scene_identity;
    let observed_scene_identity =
        ready_arena_render_identity(scene.as_deref(), &scene_markers, state.arena_index);
    let (scene_root_count, ready_scene_instance_count, scene_instances_ready) =
        arena_scene_instance_readiness(&scene_spawner, arena_scene_instances.iter());
    let observed_fighters = observed_active_fighter_count(&state, &all_fighters);
    let mut observed_combatant_bots = 0;
    for (fighter, controller, brain) in &bot_brains {
        if fighter.id < 4 && state.fighter_active(fighter.id) {
            if controller.is_bot() && matches!(brain.behavior, BotBehaviorMode::Combatant) {
                observed_combatant_bots += 1;
            }
        }
    }
    let current_arena = &arena_definitions()[state.arena_index];
    let next = PresentFixtureEvidence {
        epoch,
        scene_identity: observed_scene_identity,
        expected_fighters: run.fixture_expected_fighters,
        observed_fighters,
        expected_combatant_bots: run.fixture_expected_combatant_bots,
        observed_combatant_bots,
        expected_items: current_arena.item_anchors.len(),
        observed_items: arena_items.iter().count(),
        expected_hazard_markers: expected_hazard_marker_count(state.arena_index),
        observed_hazard_markers: arena_hazards.iter().count(),
        scene_root_count,
        ready_scene_instance_count,
        scene_root_readiness_non_vacuous: scene_root_count > 0,
        scene_instances_ready,
        valid: observed_scene_identity == expected_scene_identity && scene_instances_ready,
    };
    *evidence = PresentFixtureEvidence {
        valid: next.valid && next.counts_are_exact(),
        ..next
    };
}

/// Mirrors the public `ArenaHazardMarker` spawn contract. Saw blades and Vent
/// Spiral's reactor vents use specialized private visual components instead of
/// this marker; the scene-generation fence proves those specialized branches.
fn expected_hazard_marker_count(arena_index: usize) -> usize {
    arena_definitions()[arena_index]
        .hazards
        .iter()
        .filter(|hazard| {
            hazard.kind != ArenaHazardKind::SawBlade
                && !(arena_index == VENT_SPIRAL_ARENA_INDEX
                    && hazard.kind == ArenaHazardKind::PulseVent)
        })
        .count()
}

fn observed_active_fighter_count<'a>(
    state: &MatchState,
    fighters: impl IntoIterator<Item = &'a Fighter>,
) -> usize {
    fighters
        .into_iter()
        .filter(|fighter| state.fighter_active(fighter.id))
        .count()
}

fn map_cycle_requires_drive(run: &ScenarioRun) -> bool {
    if run.config.scenario != PerformanceScenario::MapCycle100 {
        return false;
    }
    let switches_incomplete = match run.phase {
        MeasurementPhase::Warmup => run.map_warm_precycle_switches < MAP_WARM_PRECYCLE_SWITCH_COUNT,
        MeasurementPhase::Measure => run.map_switches < MAP_SWITCH_COUNT,
        _ => false,
    };
    switches_incomplete
        || ((run.phase == MeasurementPhase::Warmup || run.phase == MeasurementPhase::Measure)
            && (run.pending_switch_started.is_some() || run.pending_present_epoch.is_some()))
}

#[derive(SystemParam)]
struct PerformanceRuntimePolicy<'w, 's> {
    commands: Commands<'w, 's>,
    asset_server: Res<'w, AssetServer>,
    preload: Option<Res<'w, MapCycleAssetPreload>>,
    scene_spawner: Res<'w, SceneSpawner>,
    arena_scene_instances:
        Query<'w, 's, Option<&'static SceneInstance>, (With<SceneRoot>, With<ArenaGeometry>)>,
    primary_windows: Query<'w, 's, &'static Window, With<PrimaryWindow>>,
    winit_settings: Res<'w, WinitSettings>,
}

impl PerformanceRuntimePolicy<'_, '_> {
    fn arena_scene_instance_readiness(&self) -> (usize, usize, bool) {
        arena_scene_instance_readiness(&self.scene_spawner, self.arena_scene_instances.iter())
    }

    fn present_mode_policy_valid(&self, uncapped: bool) -> bool {
        self.primary_windows
            .single()
            .is_ok_and(|window| window.present_mode == expected_present_mode(uncapped))
    }
}

#[allow(clippy::too_many_arguments)]
fn configure_and_drive_scenario(
    mut runtime: PerformanceRuntimePolicy,
    mut run: ResMut<ScenarioRun>,
    mut setup: ResMut<LocalSetup>,
    mut state: ResMut<MatchState>,
    mut active_arena: ResMut<ActiveArena>,
    mut user_mode: ResMut<UserModeState>,
    mut fence: ResMut<SurfacePresentFence>,
    fixture_evidence: Res<PresentFixtureEvidence>,
    scene: Option<Res<ArenaScene>>,
    geometry: Query<Entity, With<ArenaGeometry>>,
    scene_markers: Query<&ArenaSceneReadyMarker>,
    arena_items: Query<(), With<ArenaItem>>,
    arena_hazards: Query<(), With<ArenaHazardMarker>>,
    all_fighters: Query<&Fighter>,
    mut bot_brains: Query<(&Fighter, &Controller, &SimPosition, &mut BotBrain)>,
) {
    run.classify_current_frame_transition =
        run.pending_switch_started.is_some() || run.pending_present_epoch.is_some();
    if run.phase == MeasurementPhase::Configure {
        run.continuous_update_mode_valid = continuous_update_mode_valid(&runtime.winit_settings);
        if !run.continuous_update_mode_valid {
            run.phase = MeasurementPhase::AwaitSimulationReady;
            run.phase_started = Instant::now();
            run.failure = Some("performance_winit_update_mode_not_continuous");
            return;
        }
        run.present_mode_policy_valid =
            runtime.present_mode_policy_valid(run.config.uncapped_present_mode);
        if !run.present_mode_policy_valid {
            run.phase = MeasurementPhase::AwaitSimulationReady;
            run.phase_started = Instant::now();
            run.failure = Some("performance_present_mode_policy_mismatch");
            return;
        }
        if run.config.scenario == PerformanceScenario::MapCycle100 {
            if arena_definitions().len() != MAP_WARM_PRECYCLE_SWITCH_COUNT {
                run.phase = MeasurementPhase::AwaitSimulationReady;
                run.phase_started = Instant::now();
                run.failure = Some("map_cycle_arena_count_changed");
                return;
            }
            match MapCycleAssetPreload::request(&runtime.asset_server) {
                Ok(preload) => {
                    run.map_cycle_preload_asset_count = preload.handles.len();
                    runtime.commands.insert_resource(preload);
                }
                Err(error) => {
                    error!("map-cycle asset preload catalog failed: {error}");
                    run.phase = MeasurementPhase::AwaitSimulationReady;
                    run.phase_started = Instant::now();
                    run.failure = Some("map_cycle_asset_preload_catalog_failed");
                    return;
                }
            }
        }
        // Keep the real HUD/camera/audio/VFX systems installed while removing
        // menu-flow mutation from a release-equivalent graphical capture.
        user_mode.force_performance_dev_mode();
        let arena_index = match run.config.scenario {
            PerformanceScenario::MapCycle100 => 0,
            PerformanceScenario::FourBotStress | PerformanceScenario::Soak10Minutes => {
                BUMPER_ALLEY_ARENA_INDEX
            }
        };

        if run.config.scenario.uses_four_bots() {
            for slot in &mut setup.slots {
                slot.participant = ParticipantKind::Bot;
                slot.input = LocalInputAssignment::Unassigned;
            }
        }

        setup.arena_index = arena_index;
        setup.replay_seed = run.config.seed;
        state.rules = MatchRules {
            preset: RulePreset::FreeForAll,
            label: "Performance fixture",
            time_limit: None,
            starting_stocks: Some(PERF_STOCKS),
            team_scoring: false,
            friendly_fire: true,
        };
        state.rule_index = 1;
        state.arena_index = arena_index;
        state.replay_seed = run.config.seed;
        state.apply_local_setup(&setup);
        // Request the ordinary match lifecycle instead of writing a phase.
        // This keeps teardown, fighter reset, HUD, music, and arena work on the
        // exact path a local rematch uses.
        state.request_rematch();
        active_arena.select(arena_index);
        run.phase = MeasurementPhase::AwaitSimulationReady;
        run.phase_started = Instant::now();

        let arena = &arena_definitions()[arena_index];
        info!(
            "AFC_PERF_CONFIGURED scenario={} arena_index={} arena={:?} fighters={} items={} hazards={} seed=0x{:016x}",
            run.config.scenario.label(),
            arena_index,
            arena.name,
            state.active_fighter_count,
            arena.item_anchors.len(),
            arena.hazards.len(),
            run.config.seed
        );
        return;
    }

    if run.phase == MeasurementPhase::AwaitSimulationReady {
        let preload_status = if run.config.scenario == PerformanceScenario::MapCycle100 {
            runtime
                .preload
                .as_deref()
                .map_or(AssetPreloadStatus::Pending, |preload| {
                    preload.status(&runtime.asset_server)
                })
        } else {
            AssetPreloadStatus::Ready
        };
        match preload_status {
            AssetPreloadStatus::Failed => {
                run.failure = Some("map_cycle_asset_preload_failed");
                return;
            }
            AssetPreloadStatus::Ready => run.map_cycle_preload_ready = true,
            AssetPreloadStatus::Pending => run.map_cycle_preload_ready = false,
        }
        // Prove the canonical spawn and the complete logical/render fixture
        // before enabling combat AI. Promotion is latched: once bots begin
        // moving, readiness must not become impossible merely because they no
        // longer equal their spawn transforms.
        let uses_four_bots = run.config.scenario.uses_four_bots();
        let mut combatants = 0;
        let mut canonical_spawn_ready_bots = 0;
        for (fighter, controller, position, brain) in &mut bot_brains {
            if fighter.id < 4
                && state.fighter_active(fighter.id)
                && controller.is_bot()
                && matches!(brain.behavior, BotBehaviorMode::Combatant)
            {
                combatants += 1;
            }
            if fighter.id < 4 && state.fighter_active(fighter.id) {
                if controller.is_bot()
                    && state.fighter_can_participate(fighter.id)
                    && position.translation == fighter.spawn
                {
                    canonical_spawn_ready_bots += 1;
                }
            }
        }
        let active_fighters = observed_active_fighter_count(&state, &all_fighters);
        let arena = &arena_definitions()[state.arena_index];
        run.fixture_expected_fighters = state.active_fighter_count;
        run.fixture_observed_fighters = active_fighters;
        run.fixture_expected_combatant_bots = if uses_four_bots { 4 } else { 0 };
        run.fixture_expected_items = arena.item_anchors.len();
        run.fixture_observed_items = arena_items.iter().count();
        run.fixture_expected_hazard_markers = expected_hazard_marker_count(state.arena_index);
        run.fixture_observed_hazard_markers = arena_hazards.iter().count();
        let logical_counts_valid = run.fixture_observed_fighters == run.fixture_expected_fighters
            && run.fixture_observed_items == run.fixture_expected_items
            && run.fixture_observed_hazard_markers == run.fixture_expected_hazard_markers;
        let scene_identity =
            ready_arena_render_identity(scene.as_deref(), &scene_markers, state.arena_index);
        let (scene_root_count, ready_scene_instance_count, scene_instances_ready) =
            runtime.arena_scene_instance_readiness();
        run.scene_root_count = scene_root_count;
        run.ready_scene_instance_count = ready_scene_instance_count;
        run.scene_root_readiness_non_vacuous = scene_root_count > 0;
        run.scene_instances_ready = scene_instances_ready;
        let scene_ready = scene_identity.is_some() && !geometry.is_empty() && scene_instances_ready;
        let fixture_ready = !uses_four_bots
            || (state.arena_index == BUMPER_ALLEY_ARENA_INDEX
                && state.active_fighter_count == 4
                && state.active_slots.iter().all(|active| *active)
                && setup
                    .slots
                    .iter()
                    .all(|slot| slot.participant == ParticipantKind::Bot));
        let logical_scene_fixture_ready = state.phase == MatchPhase::Fighting
            && scene_ready
            && fixture_ready
            && run.map_cycle_preload_ready
            && logical_counts_valid;
        if uses_four_bots
            && !run.bots_promoted_from_canonical_fixture
            && logical_scene_fixture_ready
            && canonical_spawn_ready_bots == 4
        {
            for (fighter, _, _, mut brain) in &mut bot_brains {
                if fighter.id < 4 && state.fighter_active(fighter.id) {
                    start_bot_combat_ai(&mut brain);
                }
            }
            run.bots_promoted_from_canonical_fixture = true;
            combatants = 0;
            for (fighter, controller, _, brain) in &mut bot_brains {
                if fighter.id < 4
                    && state.fighter_active(fighter.id)
                    && controller.is_bot()
                    && matches!(brain.behavior, BotBehaviorMode::Combatant)
                {
                    combatants += 1;
                }
            }
        }
        run.fixture_observed_combatant_bots = combatants;
        run.fixture_counts_valid = logical_counts_valid
            && (!uses_four_bots
                || run.fixture_observed_combatant_bots == run.fixture_expected_combatant_bots);
        let bots_ready =
            !uses_four_bots || (run.bots_promoted_from_canonical_fixture && combatants == 4);
        if logical_scene_fixture_ready && bots_ready && run.fixture_counts_valid {
            run.readiness_valid = true;
            run.pending_scene_identity = scene_identity;
            run.pending_present_epoch = Some(fence.request());
            run.phase = MeasurementPhase::AwaitInitialPresent;
            run.phase_started = Instant::now();
        }
        return;
    }

    if run.phase == MeasurementPhase::AwaitInitialPresent {
        let pending_scene_identity = run.pending_scene_identity;
        let (_, _, scene_instances_ready) = runtime.arena_scene_instance_readiness();
        let exact_scene_ready = pending_scene_identity.is_some_and(|identity| {
            ready_arena_render_identity(scene.as_deref(), &scene_markers, identity.0)
                == Some(identity)
        }) && scene_instances_ready;
        let acknowledged_epoch = run.pending_present_epoch.filter(|epoch| {
            fence.has_observed(*epoch) && fixture_evidence.proves(*epoch, pending_scene_identity)
        });
        if exact_scene_ready && let Some(epoch) = acknowledged_epoch {
            run.apply_present_fixture_evidence(*fixture_evidence);
            run.pending_present_epoch = None;
            run.pending_scene_identity = None;
            run.fence_valid = true;
            run.initial_present_ack_count += 1;
            run.present_ack_count += 1;
            run.final_present_observed_epoch = epoch;
            run.phase = MeasurementPhase::Warmup;
            run.phase_started = Instant::now();
        }
        return;
    }

    if !map_cycle_requires_drive(&run) {
        return;
    }

    if run.pending_switch_started.is_some() && run.pending_present_epoch.is_none() {
        let scene_identity =
            ready_arena_render_identity(scene.as_deref(), &scene_markers, state.arena_index);
        let arena = &arena_definitions()[state.arena_index];
        let exact_fixture_counts = arena_items.iter().count() == arena.item_anchors.len()
            && arena_hazards.iter().count() == expected_hazard_marker_count(state.arena_index);
        let (_, _, scene_instances_ready) = runtime.arena_scene_instance_readiness();
        if state.phase == MatchPhase::Fighting
            && !geometry.is_empty()
            && scene_identity.is_some()
            && scene_instances_ready
            && exact_fixture_counts
        {
            run.pending_scene_identity = scene_identity;
            let epoch = fence.request();
            run.pending_present_epoch = Some(epoch);
            if run.phase == MeasurementPhase::Measure && run.map_switches == MAP_SWITCH_COUNT {
                run.final_present_requested_epoch = epoch;
            }
        }
        return;
    }

    if let Some(epoch) = run.pending_present_epoch {
        let pending_scene_identity = run.pending_scene_identity;
        let (_, _, scene_instances_ready) = runtime.arena_scene_instance_readiness();
        let exact_scene_ready = pending_scene_identity.is_some_and(|identity| {
            ready_arena_render_identity(scene.as_deref(), &scene_markers, identity.0)
                == Some(identity)
        }) && scene_instances_ready;
        if exact_scene_ready
            && fence.has_observed(epoch)
            && fixture_evidence.proves(epoch, pending_scene_identity)
        {
            run.apply_present_fixture_evidence(*fixture_evidence);
            run.pending_present_epoch = None;
            run.pending_scene_identity = None;
            run.fence_valid = true;
            run.present_ack_count += 1;
            run.final_present_observed_epoch = epoch;
            if let Some(started) = run.pending_switch_started.take() {
                if run.phase == MeasurementPhase::Warmup {
                    run.map_warm_precycle_present_ack_count += 1;
                    run.map_warm_precycle_valid = run.map_warm_precycle_switches
                        == MAP_WARM_PRECYCLE_SWITCH_COUNT
                        && run.map_warm_precycle_present_ack_count
                            == MAP_WARM_PRECYCLE_SWITCH_COUNT
                        && state.arena_index == 0;
                } else {
                    run.switch_ms
                        .push(started.elapsed().as_secs_f64() * 1_000.0);
                    run.map_measured_present_ack_count += 1;
                    if run.map_measured_present_ack_count % arena_definitions().len() == 0
                        && state.arena_index == 0
                    {
                        let cycle = run.map_measured_present_ack_count / arena_definitions().len();
                        run.map_checkpoint_pending = Some((cycle, epoch));
                    }
                    if run.map_measured_present_ack_count == MAP_SWITCH_COUNT {
                        run.final_present_ack_count = 1;
                        run.final_present_requested_epoch = epoch;
                        run.final_present_observed_epoch = epoch;
                    }
                }
            }
        }
        return;
    }

    if run.pending_switch_started.is_some() {
        return;
    }

    let (switch_count, switch_target, elapsed_seconds, interval_seconds) =
        if run.phase == MeasurementPhase::Warmup {
            (
                run.map_warm_precycle_switches,
                MAP_WARM_PRECYCLE_SWITCH_COUNT,
                run.warmup_elapsed,
                run.config.warmup_seconds / MAP_WARM_PRECYCLE_SWITCH_COUNT as f64,
            )
        } else {
            (
                run.map_switches,
                MAP_SWITCH_COUNT,
                run.measurement_elapsed,
                run.config.measurement_seconds / MAP_SWITCH_COUNT as f64,
            )
        };
    let next_switch_at = (switch_count + 1) as f64 * interval_seconds;
    if switch_count < switch_target && elapsed_seconds >= next_switch_at {
        let next_arena = (state.arena_index + 1) % arena_definitions().len();
        setup.arena_index = next_arena;
        state.arena_index = next_arena;
        active_arena.select(next_arena);
        state.request_rematch();
        if run.phase == MeasurementPhase::Warmup {
            run.map_warm_precycle_switches += 1;
        } else {
            run.map_switches += 1;
            run.classify_current_frame_transition = true;
        }
        run.pending_switch_started = Some(Instant::now());
        // The following frame must pass through normal reset + arena rebuild.
        // The exact epoch fence is requested only once that lifecycle has
        // restored both fighting state and ArenaScene readiness.
    }
}

fn record_periodic_process_sample(run: &mut ScenarioRun, sample: ProcessSample) {
    let elapsed_seconds = run.measurement_elapsed;
    run.last_resident_gib = sample.resident_gib;
    run.process_memory_gib.push(sample.resident_gib);
    run.process_memory_observations
        .push(ProcessMemoryObservation {
            elapsed_seconds,
            resident_gib: sample.resident_gib,
        });
    if let Some((previous_wall_seconds, previous_cpu_seconds)) = run.previous_process_cpu_sample {
        let wall_seconds = elapsed_seconds - previous_wall_seconds;
        if wall_seconds > 0.0 {
            run.process_cpu_percent
                .push((sample.cpu_seconds - previous_cpu_seconds).max(0.0) / wall_seconds * 100.0);
        }
    }
    run.previous_process_cpu_sample = Some((elapsed_seconds, sample.cpu_seconds));
}

fn warmup_fixture_ready(run: &ScenarioRun) -> bool {
    run.config.scenario != PerformanceScenario::MapCycle100
        || (run.map_warm_precycle_valid
            && run.map_warm_precycle_switches == MAP_WARM_PRECYCLE_SWITCH_COUNT
            && run.map_warm_precycle_present_ack_count == MAP_WARM_PRECYCLE_SWITCH_COUNT
            && run.pending_switch_started.is_none()
            && run.pending_present_epoch.is_none())
}

#[allow(clippy::too_many_arguments)]
fn collect_scenario_metrics(
    mut run: ResMut<ScenarioRun>,
    time: Res<Time<Real>>,
    entities: &Entities,
    meshes: Option<Res<Assets<Mesh>>>,
    materials: Option<Res<Assets<StandardMaterial>>>,
    images: Option<Res<Assets<Image>>>,
    diagnostics: Res<DiagnosticsStore>,
    mut fence: ResMut<SurfacePresentFence>,
    fixture_evidence: Res<PresentFixtureEvidence>,
    state: Res<MatchState>,
    scene: Option<Res<ArenaScene>>,
    scene_markers: Query<&ArenaSceneReadyMarker>,
    journal: Res<crate::sim_event::SimEventJournal>,
    event_buffer: Res<crate::sim_event::TickEventBuffer>,
    mut owner_work: ParamSet<(
        Query<&Hitbox>,
        Query<&ActiveSpecial>,
        Query<&ActiveBeeSkill>,
        Query<&ActiveChickSkill>,
        Query<&ActivePenguinSkill>,
        Query<&ActivePenguinSurface>,
    )>,
) {
    let delta_seconds = time.delta_secs_f64();
    if matches!(
        run.phase,
        MeasurementPhase::Warmup | MeasurementPhase::Measure
    ) && run
        .pending_switch_started
        .is_some_and(|started| started.elapsed() > READINESS_TIMEOUT)
    {
        run.phase = MeasurementPhase::Failed;
        run.failure = Some("map_cycle_readiness_or_present_timeout");
        report_result(&run, &diagnostics);
        run.request_exit(AppExit::error());
        return;
    }
    match run.phase {
        MeasurementPhase::Configure | MeasurementPhase::Complete | MeasurementPhase::Failed => {
            return;
        }
        MeasurementPhase::AwaitSimulationReady
        | MeasurementPhase::AwaitInitialPresent
        | MeasurementPhase::AwaitFinalPresent => {
            if run.failure.is_some() {
                report_result(&run, &diagnostics);
                run.phase = MeasurementPhase::Failed;
                run.request_exit(AppExit::error());
                return;
            }
            let pending_scene_identity = run.pending_scene_identity;
            let exact_scene_ready = pending_scene_identity.is_some_and(|identity| {
                ready_arena_render_identity(scene.as_deref(), &scene_markers, identity.0)
                    == Some(identity)
            });
            let acknowledged_epoch = run.pending_present_epoch.filter(|epoch| {
                fence.has_observed(*epoch)
                    && fixture_evidence.proves(*epoch, pending_scene_identity)
            });
            if run.phase == MeasurementPhase::AwaitFinalPresent
                && exact_scene_ready
                && let Some(epoch) = acknowledged_epoch
            {
                run.apply_present_fixture_evidence(*fixture_evidence);
                run.pending_present_epoch = None;
                run.pending_scene_identity = None;
                run.fence_valid = true;
                run.final_present_ack_count += 1;
                run.present_ack_count += 1;
                run.final_present_observed_epoch = epoch;
                let valid = scenario_valid(&run);
                report_result(&run, &diagnostics);
                run.phase = if valid {
                    MeasurementPhase::Complete
                } else {
                    MeasurementPhase::Failed
                };
                run.request_exit(if valid {
                    AppExit::Success
                } else {
                    AppExit::error()
                });
                return;
            }
            if run.phase_started.elapsed() > READINESS_TIMEOUT {
                run.phase = MeasurementPhase::Failed;
                run.failure = Some("simulation_or_render_readiness_timeout");
                report_result(&run, &diagnostics);
                run.request_exit(AppExit::error());
            }
            return;
        }
        MeasurementPhase::Warmup => {
            run.warmup_elapsed += delta_seconds;
            if run.warmup_elapsed >= run.config.warmup_seconds && warmup_fixture_ready(&run) {
                run.begin_measurement(event_buffer.overflow_count(), journal.newest_tick());
            }
            return;
        }
        MeasurementPhase::Measure => {}
    }

    run.measurement_elapsed += delta_seconds;
    run.frame_ms.push(delta_seconds * 1_000.0);
    if let Some(started) = run.frame_cpu_started.take() {
        run.cpu_frame_ms
            .push(started.elapsed().as_secs_f64() * 1_000.0);
    }
    if run.measurement_elapsed >= run.config.measurement_seconds {
        run.frame_classification.finalization += 1;
    } else if run.classify_current_frame_transition {
        run.frame_classification.transition += 1;
    } else {
        run.frame_classification.steady += 1;
    }
    run.classify_current_frame_transition = false;

    let entity_count = entities.count_spawned() as usize;
    let mesh_count = meshes.as_ref().map_or(0, |assets| assets.len());
    let material_count = materials.as_ref().map_or(0, |assets| assets.len());
    let image_count = images.as_ref().map_or(0, |assets| assets.len());
    let resources = ResourceCounts {
        entities: entity_count,
        meshes: mesh_count,
        materials: material_count,
        images: image_count,
    };
    run.resource_start.get_or_insert(resources);
    run.entity_peak = run.entity_peak.max(entity_count);
    run.entity_end = entity_count;
    run.mesh_peak = run.mesh_peak.max(mesh_count);
    run.mesh_end = mesh_count;
    run.material_peak = run.material_peak.max(material_count);
    run.material_end = material_count;
    run.image_peak = run.image_peak.max(image_count);
    run.image_end = image_count;

    // Inspect every journal tick since the previous render frame. This detects
    // event-history holes even when multiple fixed ticks happen in one frame.
    if let Some(newest) = journal.newest_tick()
        && run.last_activity_tick != Some(newest)
    {
        let first = run
            .last_activity_tick
            .map_or(newest.get(), |previous| previous.get().saturating_add(1));
        for raw_tick in first..=newest.get() {
            let tick = crate::simulation::SimTick(raw_tick);
            let Some(events) = journal.events_at(tick) else {
                run.journal_gap_ticks = run.journal_gap_ticks.saturating_add(1);
                continue;
            };
            for event in events.iter().flatten() {
                use crate::sim_event::{SimEventKind, SimEventSource};
                let source_owner = match event.id.source {
                    SimEventSource::Fighter(fighter) => Some(fighter.index()),
                    _ => None,
                };
                let (owner, kind) = match event.kind {
                    SimEventKind::ActionStarted { fighter, .. } => (Some(fighter.index()), 0u8),
                    SimEventKind::HitConfirmed { attacker, .. } => {
                        (attacker.map(|fighter| fighter.index()), 1)
                    }
                    SimEventKind::Guarded { attacker, .. } => {
                        (attacker.map(|fighter| fighter.index()), 2)
                    }
                    SimEventKind::ItemLifecycle { fighter, .. } => {
                        (fighter.map(|fighter| fighter.index()), 3)
                    }
                    SimEventKind::AbilityLifecycle { .. } => (source_owner, 4),
                    _ => (None, u8::MAX),
                };
                if let Some(owner) = owner.filter(|owner| *owner < 4) {
                    let activity = &mut run.activity[owner];
                    match kind {
                        0 => activity.actions = activity.actions.saturating_add(1),
                        1 => activity.hits = activity.hits.saturating_add(1),
                        2 => activity.guards = activity.guards.saturating_add(1),
                        3 => activity.items = activity.items.saturating_add(1),
                        4 => activity.abilities = activity.abilities.saturating_add(1),
                        _ => {}
                    }
                }
            }
        }
        run.last_activity_tick = Some(newest);
    }
    run.event_overflow_end = event_buffer.overflow_count();
    let mut workload = [OwnerWorkload::default(); 4];
    let mut stale_owner_entities = 0_usize;
    let mut count_owner = |owner: usize, selector: fn(&mut OwnerWorkload) -> &mut usize| {
        if owner < workload.len() && state.fighter_active(owner) {
            *selector(&mut workload[owner]) += 1;
        } else {
            stale_owner_entities += 1;
        }
    };
    for hitbox in &owner_work.p0() {
        count_owner(hitbox.owner.index(), |work| &mut work.hitboxes);
    }
    for special in &owner_work.p1() {
        count_owner(special.owner.index(), |work| &mut work.specials);
    }
    for skill in &owner_work.p2() {
        count_owner(skill.owner.index(), |work| &mut work.bee_skills);
    }
    for skill in &owner_work.p3() {
        count_owner(skill.owner.index(), |work| &mut work.chick_skills);
    }
    for skill in &owner_work.p4() {
        count_owner(skill.owner.index(), |work| &mut work.penguin_skills);
    }
    for surface in &owner_work.p5() {
        count_owner(surface.owner.index(), |work| &mut work.penguin_surfaces);
    }
    for owner in 0..workload.len() {
        run.owner_end[owner] = workload[owner];
        run.owner_peak[owner].max_assign(workload[owner]);
    }
    run.stale_owner_entities_end = stale_owner_entities;
    run.stale_owner_entities_peak = run.stale_owner_entities_peak.max(stale_owner_entities);

    let periodic_process_sample_due = run.measurement_elapsed >= run.next_system_sample_seconds;
    let exact_map_checkpoint_due = run.map_checkpoint_pending.is_some();
    let process_sample = (periodic_process_sample_due || exact_map_checkpoint_due)
        .then(process_sample)
        .flatten();
    if periodic_process_sample_due {
        if let Some(sample) = process_sample {
            record_periodic_process_sample(&mut run, sample);
        }
        run.next_system_sample_seconds = run.measurement_elapsed.floor() + 1.0;
    }

    if run.measurement_elapsed >= run.next_resource_sample_seconds {
        let live_bytes = allocation_snapshot().live_bytes;
        let elapsed_seconds = run.measurement_elapsed;
        let resident_gib = run.last_resident_gib;
        run.resource_growth.push(ResourceGrowthSample {
            elapsed_seconds,
            resources,
            resident_gib,
            live_bytes,
            stale_owner_entities,
        });
        run.next_resource_sample_seconds += run.resource_sample_interval_seconds;
    }

    if let Some((cycle, present_epoch)) = run.map_checkpoint_pending.take() {
        let expected_switches = cycle.saturating_mul(arena_definitions().len());
        let checkpoint_valid = run.config.scenario == PerformanceScenario::MapCycle100
            && cycle < MAP_ALIGNED_CHECKPOINT_COUNT
            && run.map_cycle_checkpoints.len() == cycle
            && present_epoch != 0
            && state.arena_index == 0
            && run.pending_switch_started.is_none()
            && run.pending_present_epoch.is_none()
            && run.map_measured_present_ack_count == expected_switches
            && (cycle != 0 || run.map_warm_precycle_valid);
        if !checkpoint_valid {
            run.phase = MeasurementPhase::Failed;
            run.failure = Some("map_cycle_aligned_checkpoint_invalid");
            report_result(&run, &diagnostics);
            run.request_exit(AppExit::error());
            return;
        }
        if cycle == 1 {
            run.map_first_cycle_resources = Some(resources);
        }
        let checkpoint = MapCycleCheckpoint {
            cycle,
            present_epoch,
            elapsed_seconds: run.measurement_elapsed,
            resources,
            resident_gib: process_sample.map(|sample| sample.resident_gib),
            live_bytes: allocation_snapshot().live_bytes,
        };
        run.map_cycle_checkpoints.push(checkpoint);
    }

    let map_cycle_complete = run.config.scenario != PerformanceScenario::MapCycle100
        || (run.map_switches >= MAP_SWITCH_COUNT
            && run.pending_switch_started.is_none()
            && run.pending_present_epoch.is_none());
    if run.measurement_elapsed < run.config.measurement_seconds || !map_cycle_complete {
        return;
    }

    let final_growth_sample = ResourceGrowthSample {
        elapsed_seconds: run.measurement_elapsed,
        resources,
        resident_gib: run.last_resident_gib,
        live_bytes: allocation_snapshot().live_bytes,
        stale_owner_entities,
    };
    run.resource_growth.push(final_growth_sample);

    if run.config.scenario != PerformanceScenario::MapCycle100 {
        let Some(identity) = ready_arena_render_identity(
            scene.as_deref(),
            &scene_markers,
            run.config
                .scenario
                .uses_four_bots()
                .then_some(BUMPER_ALLEY_ARENA_INDEX)
                .unwrap_or(0),
        ) else {
            run.phase = MeasurementPhase::Failed;
            run.failure = Some("final_arena_scene_generation_not_ready");
            report_result(&run, &diagnostics);
            run.request_exit(AppExit::error());
            return;
        };
        run.pending_scene_identity = Some(identity);
        let epoch = fence.request();
        run.pending_present_epoch = Some(epoch);
        run.final_present_requested_epoch = epoch;
        run.phase = MeasurementPhase::AwaitFinalPresent;
        run.phase_started = Instant::now();
        return;
    }
    let valid = scenario_valid(&run);
    report_result(&run, &diagnostics);
    run.phase = if valid {
        MeasurementPhase::Complete
    } else {
        MeasurementPhase::Failed
    };
    run.request_exit(if valid {
        AppExit::Success
    } else {
        AppExit::error()
    });
}

fn activity_valid(run: &ScenarioRun) -> bool {
    if !run.config.scenario.uses_four_bots() {
        return true;
    }
    combat_activity_owner_valid(run).iter().all(|valid| *valid)
        && journal_continuity_valid(run)
        && event_continuity_valid(run)
}

fn combat_activity_floors(run: &ScenarioRun) -> (usize, usize) {
    // Floors scale with the declared measurement, not frame rate. They prove
    // each owner performed meaningful combat over a short test override while
    // still catching a dormant bot during the five/ten-minute release runs.
    let total_floor = (run.config.measurement_seconds / 30.0)
        .ceil()
        .max(MIN_COMBAT_ACTIVITY_PER_OWNER as f64) as usize;
    let hit_floor = (run.config.measurement_seconds / 120.0).ceil().max(1.0) as usize;
    (total_floor, hit_floor)
}

fn combat_activity_owner_valid(run: &ScenarioRun) -> [bool; 4] {
    let (total_floor, hit_floor) = combat_activity_floors(run);
    run.activity
        .map(|activity| activity.total() >= total_floor && activity.hits >= hit_floor)
}

fn journal_continuity_valid(run: &ScenarioRun) -> bool {
    run.journal_gap_ticks == 0
}

fn event_continuity_valid(run: &ScenarioRun) -> bool {
    run.event_overflow_end == run.event_overflow_start
}

fn owner_workload_owner_valid(run: &ScenarioRun) -> [bool; 4] {
    run.owner_peak.map(|workload| workload.total() > 0)
}

fn owner_valid(run: &ScenarioRun) -> bool {
    !run.config.scenario.uses_four_bots()
        || owner_workload_owner_valid(run).iter().all(|valid| *valid)
}

#[derive(Clone, Copy, Debug, Default)]
struct ResourceGrowthAnalysis {
    sample_count: usize,
    stale_owner_sample_peak: usize,
    entity_slope_per_minute: f64,
    mesh_slope_per_minute: f64,
    material_slope_per_minute: f64,
    image_slope_per_minute: f64,
    resident_mib_slope_per_minute: f64,
    live_mib_slope_per_minute: f64,
    entity_monotonic_growth: bool,
    mesh_monotonic_growth: bool,
    material_monotonic_growth: bool,
    image_monotonic_growth: bool,
    resident_monotonic_growth: bool,
    live_monotonic_growth: bool,
    final_asset_window_plateau: bool,
    final_resident_window_range_mib: f64,
    final_resident_window_slope_mib_per_minute: f64,
    final_resident_window_plateau: bool,
    final_live_window_range_mib: f64,
    final_live_window_slope_mib_per_minute: f64,
    final_live_window_plateau: bool,
}

fn growth_slope_per_minute(
    growth: &BoundedResourceGrowth,
    value: impl Fn(ResourceGrowthSample) -> f64 + Copy,
) -> f64 {
    let count = growth.len();
    if count < 2 {
        return 0.0;
    }
    let count_f64 = count as f64;
    let mean_x = growth
        .iter()
        .map(|sample| sample.elapsed_seconds / 60.0)
        .sum::<f64>()
        / count_f64;
    let mean_y = growth.iter().copied().map(value).sum::<f64>() / count_f64;
    let (numerator, denominator) =
        growth
            .iter()
            .copied()
            .fold((0.0, 0.0), |(numerator, denominator), sample| {
                let x = sample.elapsed_seconds / 60.0 - mean_x;
                (
                    numerator + x * (value(sample) - mean_y),
                    denominator + x * x,
                )
            });
    if denominator > 0.0 {
        numerator / denominator
    } else {
        0.0
    }
}

fn final_window_stats(
    growth: &BoundedResourceGrowth,
    window_seconds: f64,
    value: impl Fn(ResourceGrowthSample) -> f64 + Copy,
) -> (usize, f64, f64) {
    let last_elapsed = growth.last().map_or(0.0, |sample| sample.elapsed_seconds);
    let start = (last_elapsed - window_seconds).max(0.0);
    let selected = || {
        growth
            .iter()
            .copied()
            .filter(|sample| sample.elapsed_seconds >= start)
    };
    let count = selected().count();
    if count < 2 {
        return (count, 0.0, 0.0);
    }
    let count_f64 = count as f64;
    let mean_x = selected()
        .map(|sample| sample.elapsed_seconds / 60.0)
        .sum::<f64>()
        / count_f64;
    let mean_y = selected().map(&value).sum::<f64>() / count_f64;
    let (minimum, maximum, numerator, denominator) = selected().fold(
        (f64::INFINITY, f64::NEG_INFINITY, 0.0, 0.0),
        |(minimum, maximum, numerator, denominator), sample| {
            let y = value(sample);
            let x = sample.elapsed_seconds / 60.0 - mean_x;
            (
                minimum.min(y),
                maximum.max(y),
                numerator + x * (y - mean_y),
                denominator + x * x,
            )
        },
    );
    let slope = if denominator > 0.0 {
        numerator / denominator
    } else {
        0.0
    };
    (count, maximum - minimum, slope)
}

fn resource_growth_analysis(run: &ScenarioRun) -> ResourceGrowthAnalysis {
    let last_elapsed = run
        .resource_growth
        .last()
        .map_or(0.0, |sample| sample.elapsed_seconds);
    let plateau_start = (last_elapsed - RESOURCE_PLATEAU_WINDOW_SECONDS).max(0.0);
    let mut plateau = run
        .resource_growth
        .iter()
        .copied()
        .filter(|sample| sample.elapsed_seconds >= plateau_start);
    let first_plateau = plateau.next();
    let mut plateau_count = usize::from(first_plateau.is_some());
    let final_asset_window_plateau = first_plateau.is_some_and(|first| {
        plateau.all(|sample| {
            plateau_count += 1;
            sample.resources.meshes == first.resources.meshes
                && sample.resources.materials == first.resources.materials
                && sample.resources.images == first.resources.images
        }) && plateau_count >= 2
    });
    let (resident_window_count, resident_window_range, resident_window_slope) = final_window_stats(
        &run.resource_growth,
        RESOURCE_PLATEAU_WINDOW_SECONDS,
        |sample| sample.resident_gib * 1024.0,
    );
    let (live_window_count, live_window_range, live_window_slope) = final_window_stats(
        &run.resource_growth,
        RESOURCE_PLATEAU_WINDOW_SECONDS,
        |sample| sample.live_bytes as f64 / 1024.0 / 1024.0,
    );

    ResourceGrowthAnalysis {
        sample_count: run.resource_growth.len(),
        stale_owner_sample_peak: run
            .resource_growth
            .iter()
            .map(|sample| sample.stale_owner_entities)
            .max()
            .unwrap_or(0),
        entity_slope_per_minute: growth_slope_per_minute(&run.resource_growth, |sample| {
            sample.resources.entities as f64
        }),
        mesh_slope_per_minute: growth_slope_per_minute(&run.resource_growth, |sample| {
            sample.resources.meshes as f64
        }),
        material_slope_per_minute: growth_slope_per_minute(&run.resource_growth, |sample| {
            sample.resources.materials as f64
        }),
        image_slope_per_minute: growth_slope_per_minute(&run.resource_growth, |sample| {
            sample.resources.images as f64
        }),
        resident_mib_slope_per_minute: growth_slope_per_minute(&run.resource_growth, |sample| {
            sample.resident_gib * 1024.0
        }),
        live_mib_slope_per_minute: growth_slope_per_minute(&run.resource_growth, |sample| {
            sample.live_bytes as f64 / 1024.0 / 1024.0
        }),
        entity_monotonic_growth: run.resource_growth.monotonic_growth(0),
        mesh_monotonic_growth: run.resource_growth.monotonic_growth(1),
        material_monotonic_growth: run.resource_growth.monotonic_growth(2),
        image_monotonic_growth: run.resource_growth.monotonic_growth(3),
        resident_monotonic_growth: run.resource_growth.monotonic_growth(4),
        live_monotonic_growth: run.resource_growth.monotonic_growth(5),
        final_asset_window_plateau,
        final_resident_window_range_mib: resident_window_range,
        final_resident_window_slope_mib_per_minute: resident_window_slope,
        final_resident_window_plateau: resident_window_count >= 2
            && resident_window_range <= RSS_PLATEAU_RANGE_MIB
            && resident_window_slope.abs() <= RSS_PLATEAU_SLOPE_MIB_PER_MINUTE,
        final_live_window_range_mib: live_window_range,
        final_live_window_slope_mib_per_minute: live_window_slope,
        final_live_window_plateau: live_window_count >= 2
            && live_window_range <= LIVE_BYTES_PLATEAU_RANGE_MIB
            && live_window_slope.abs() <= LIVE_BYTES_PLATEAU_SLOPE_MIB_PER_MINUTE,
    }
}

fn map_cycle_checkpoint_structure_valid(run: &ScenarioRun) -> bool {
    if run.config.scenario != PerformanceScenario::MapCycle100 {
        return true;
    }
    if run.map_cycle_checkpoints.len() != MAP_ALIGNED_CHECKPOINT_COUNT {
        return false;
    }
    run.map_cycle_checkpoints
        .iter()
        .enumerate()
        .all(|(index, checkpoint)| {
            checkpoint.cycle == index
                && checkpoint.present_epoch != 0
                && (index == 0
                    || (checkpoint.present_epoch
                        > run.map_cycle_checkpoints[index - 1].present_epoch
                        && checkpoint.elapsed_seconds
                            > run.map_cycle_checkpoints[index - 1].elapsed_seconds))
        })
}

fn aligned_checkpoint_stats(
    checkpoints: &[MapCycleCheckpoint],
    value: impl Fn(&MapCycleCheckpoint) -> Option<f64>,
) -> Option<(f64, f64)> {
    if checkpoints.len() < MAP_ALIGNED_TAIL_CHECKPOINTS {
        return None;
    }
    let checkpoints = &checkpoints[checkpoints.len() - MAP_ALIGNED_TAIL_CHECKPOINTS..];
    if !checkpoints
        .windows(2)
        .all(|pair| pair[0].elapsed_seconds < pair[1].elapsed_seconds)
    {
        return None;
    }
    let values = checkpoints.iter().map(&value).collect::<Option<Vec<_>>>()?;
    let count = checkpoints.len() as f64;
    let mean_x = checkpoints
        .iter()
        .map(|checkpoint| checkpoint.elapsed_seconds / 60.0)
        .sum::<f64>()
        / count;
    let mean_y = values.iter().sum::<f64>() / count;
    let (minimum, maximum, numerator, denominator) = checkpoints.iter().zip(values).fold(
        (f64::INFINITY, f64::NEG_INFINITY, 0.0, 0.0),
        |(minimum, maximum, numerator, denominator), (checkpoint, y)| {
            let x = checkpoint.elapsed_seconds / 60.0 - mean_x;
            (
                minimum.min(y),
                maximum.max(y),
                numerator + x * (y - mean_y),
                denominator + x * x,
            )
        },
    );
    if denominator <= 0.0 {
        return None;
    }
    Some((maximum - minimum, numerator / denominator))
}

fn aligned_cycle_growth_analysis(run: &ScenarioRun) -> AlignedCycleGrowthAnalysis {
    let checkpoint_count = run.map_cycle_checkpoints.len();
    let tail_count = checkpoint_count.min(MAP_ALIGNED_TAIL_CHECKPOINTS);
    let interval_count = tail_count.saturating_sub(1);
    let span_seconds = if tail_count >= 2 {
        let tail = &run.map_cycle_checkpoints[checkpoint_count - tail_count..];
        tail.last()
            .map_or(0.0, |checkpoint| checkpoint.elapsed_seconds)
            - tail
                .first()
                .map_or(0.0, |checkpoint| checkpoint.elapsed_seconds)
    } else {
        0.0
    };
    let resource_counts_valid = map_cycle_checkpoint_structure_valid(run)
        && run.map_cycle_checkpoints.first().is_some_and(|first| {
            run.map_cycle_checkpoints
                .iter()
                .all(|checkpoint| checkpoint.resources == first.resources)
        });
    let resident_stats = aligned_checkpoint_stats(&run.map_cycle_checkpoints, |checkpoint| {
        checkpoint.resident_gib.map(|resident| resident * 1024.0)
    });
    let live_stats = aligned_checkpoint_stats(&run.map_cycle_checkpoints, |checkpoint| {
        Some(checkpoint.live_bytes as f64 / 1024.0 / 1024.0)
    });
    let (resident_range_mib, resident_slope_mib_per_minute) = resident_stats.unwrap_or_default();
    let (live_range_mib, live_slope_mib_per_minute) = live_stats.unwrap_or_default();
    AlignedCycleGrowthAnalysis {
        checkpoint_count,
        interval_count,
        span_seconds,
        resource_counts_valid,
        resident_range_mib,
        resident_slope_mib_per_minute,
        resident_plateau: interval_count >= 3
            && resident_stats.is_some()
            && resident_range_mib <= RSS_PLATEAU_RANGE_MIB
            && resident_slope_mib_per_minute.abs() <= RSS_PLATEAU_SLOPE_MIB_PER_MINUTE,
        live_range_mib,
        live_slope_mib_per_minute,
        live_plateau: interval_count >= 3
            && live_stats.is_some()
            && live_range_mib <= LIVE_BYTES_PLATEAU_RANGE_MIB
            && live_slope_mib_per_minute.abs() <= LIVE_BYTES_PLATEAU_SLOPE_MIB_PER_MINUTE,
    }
}

fn frame_classification_valid(run: &ScenarioRun) -> bool {
    run.frame_classification.valid_for(run.frame_ms.len())
}

fn map_warm_precycle_evidence_valid(run: &ScenarioRun) -> bool {
    run.config.scenario != PerformanceScenario::MapCycle100
        || (run.map_warm_precycle_valid
            && run.map_warm_precycle_switches == MAP_WARM_PRECYCLE_SWITCH_COUNT
            && run.map_warm_precycle_present_ack_count == MAP_WARM_PRECYCLE_SWITCH_COUNT)
}

fn present_acknowledgements_valid(run: &ScenarioRun) -> bool {
    if run.config.scenario == PerformanceScenario::MapCycle100 {
        run.initial_present_ack_count == 1
            && run.map_warm_precycle_present_ack_count == MAP_WARM_PRECYCLE_SWITCH_COUNT
            && run.map_measured_present_ack_count == MAP_SWITCH_COUNT
            && run.final_present_ack_count == 1
            && run.present_ack_count == 1 + MAP_WARM_PRECYCLE_SWITCH_COUNT + MAP_SWITCH_COUNT
            && run.final_present_requested_epoch != 0
            && run.final_present_requested_epoch == run.final_present_observed_epoch
    } else {
        run.initial_present_ack_count == 1
            && run.final_present_ack_count == 1
            && run.present_ack_count == 2
            && run.final_present_requested_epoch != 0
            && run.final_present_requested_epoch == run.final_present_observed_epoch
    }
}

fn map_switch_samples_valid(run: &ScenarioRun) -> bool {
    run.config.scenario != PerformanceScenario::MapCycle100
        || (run.map_switches == MAP_SWITCH_COUNT
            && run.switch_ms.len() == MAP_SWITCH_COUNT
            && run.map_measured_present_ack_count == MAP_SWITCH_COUNT)
}

fn resource_stability_valid(run: &ScenarioRun) -> bool {
    let growth = resource_growth_analysis(run);
    if run.stale_owner_entities_peak != 0 || growth.stale_owner_sample_peak != 0 {
        return false;
    }
    match run.config.scenario {
        PerformanceScenario::FourBotStress => true,
        PerformanceScenario::MapCycle100 => {
            let final_resources = ResourceCounts {
                entities: run.entity_end,
                meshes: run.mesh_end,
                materials: run.material_end,
                images: run.image_end,
            };
            run.map_first_cycle_resources == Some(final_resources)
                && aligned_cycle_growth_analysis(run).resource_counts_valid
        }
        PerformanceScenario::Soak10Minutes => {
            growth.sample_count >= 2
                && !run.process_memory_gib.is_empty()
                && growth.final_asset_window_plateau
                && !growth.entity_monotonic_growth
                && !growth.mesh_monotonic_growth
                && !growth.material_monotonic_growth
                && !growth.image_monotonic_growth
                && !growth.resident_monotonic_growth
                && growth.final_resident_window_plateau
                && (!cfg!(feature = "perf-alloc")
                    || (!growth.live_monotonic_growth && growth.final_live_window_plateau))
        }
    }
}

fn scenario_valid(run: &ScenarioRun) -> bool {
    run.failure.is_none()
        && run.continuous_update_mode_valid
        && run.present_mode_policy_valid
        && run.map_cycle_preload_ready
        && run.measurement_started
        && run.readiness_valid
        && run.fence_valid
        && run.scene_root_readiness_non_vacuous
        && run.scene_instances_ready
        && run.fixture_counts_valid
        && (!run.config.scenario.uses_four_bots() || run.bots_promoted_from_canonical_fixture)
        && frame_classification_valid(run)
        && map_warm_precycle_evidence_valid(run)
        && map_cycle_checkpoint_structure_valid(run)
        && present_acknowledgements_valid(run)
        && map_switch_samples_valid(run)
        && activity_valid(run)
        && owner_valid(run)
        && resource_stability_valid(run)
}

fn fixture_invalid_reasons(run: &ScenarioRun) -> Vec<&'static str> {
    let mut reasons = Vec::new();
    if let Some(failure) = run.failure {
        reasons.push(failure);
    }
    if !run.continuous_update_mode_valid {
        reasons.push("continuous_update_mode_invalid");
    }
    if !run.present_mode_policy_valid {
        reasons.push("present_mode_policy_invalid");
    }
    if !run.map_cycle_preload_ready {
        reasons.push("map_cycle_preload_not_ready");
    }
    if !run.measurement_started {
        reasons.push("measurement_not_started");
    }
    if !run.readiness_valid {
        reasons.push("simulation_not_ready");
    }
    if !run.fence_valid {
        reasons.push("surface_present_invocation_not_observed");
    }
    if !run.scene_root_readiness_non_vacuous {
        reasons.push("arena_scene_root_readiness_vacuous");
    }
    if !run.scene_instances_ready {
        reasons.push("arena_scene_instances_not_ready");
    }
    if !run.fixture_counts_valid {
        reasons.push("fixture_count_mismatch");
    }
    if run.config.scenario.uses_four_bots() && !run.bots_promoted_from_canonical_fixture {
        reasons.push("bots_not_promoted_from_canonical_fixture");
    }
    if !frame_classification_valid(run) {
        reasons.push("frame_classification_gap");
    }
    if !map_warm_precycle_evidence_valid(run) {
        reasons.push("map_warm_precycle_invalid");
    }
    if !map_cycle_checkpoint_structure_valid(run) {
        reasons.push("map_cycle_aligned_checkpoint_invalid");
    }
    if !present_acknowledgements_valid(run) {
        reasons.push("present_acknowledgement_count_invalid");
    }
    if !map_switch_samples_valid(run) {
        reasons.push("map_switch_sample_mismatch");
    }
    if !activity_valid(run) {
        reasons.push("combat_activity_invalid");
    }
    if !owner_valid(run) {
        reasons.push("owner_workload_invalid");
    }
    if !resource_stability_valid(run) {
        reasons.push("resource_stability_invalid");
    }
    reasons
}

fn fixture_failure(run: &ScenarioRun) -> &'static str {
    fixture_invalid_reasons(run).first().copied().unwrap_or("")
}

fn allocation_measurement_status(measurement_started: bool, instrumented: bool) -> &'static str {
    if !measurement_started {
        "unavailable_measurement_not_started"
    } else if instrumented {
        "available"
    } else {
        "unavailable_without_perf_alloc"
    }
}

fn performance_acceptance_status(
    fixture_valid: bool,
    canonical_capture_eligible: bool,
    timing_gate_pass: bool,
    rss_gate_pass: bool,
    live_gate_pass: bool,
) -> &'static str {
    if !fixture_valid {
        "fixture_invalid"
    } else if !canonical_capture_eligible {
        "exploratory_only_noncanonical_configuration"
    } else if !timing_gate_pass {
        "local_timing_budget_failed"
    } else if !rss_gate_pass {
        "rss_growth_evidence_failed"
    } else if !live_gate_pass {
        "live_growth_evidence_failed"
    } else {
        "external_gpu_evidence_required"
    }
}

fn aligned_rss_growth_acceptance_status(
    run: &ScenarioRun,
    aligned: AlignedCycleGrowthAnalysis,
) -> &'static str {
    if run.config.scenario != PerformanceScenario::MapCycle100 {
        return "not_applicable";
    }
    if !run.measurement_started {
        return "unavailable_measurement_not_started";
    }
    if run.measurement_elapsed < run.config.scenario.default_measurement_seconds() {
        return "insufficient_short_duration_evidence";
    }
    if !map_cycle_checkpoint_structure_valid(run) || aligned.interval_count < 3 {
        return "insufficient_aligned_cycle_evidence";
    }
    if run.map_cycle_checkpoints[run.map_cycle_checkpoints.len() - MAP_ALIGNED_TAIL_CHECKPOINTS..]
        .iter()
        .any(|checkpoint| checkpoint.resident_gib.is_none())
    {
        return "unavailable_on_platform";
    }
    if aligned.resident_plateau {
        "passed"
    } else {
        "failed"
    }
}

fn aligned_live_growth_evidence_status(
    run: &ScenarioRun,
    aligned: AlignedCycleGrowthAnalysis,
) -> &'static str {
    if run.config.scenario != PerformanceScenario::MapCycle100 {
        return "not_applicable";
    }
    if !run.measurement_started {
        return "unavailable_measurement_not_started";
    }
    if !cfg!(feature = "perf-alloc") {
        return "unavailable_without_perf_alloc";
    }
    if run.measurement_elapsed < run.config.scenario.default_measurement_seconds() {
        return "insufficient_short_duration_evidence";
    }
    if !map_cycle_checkpoint_structure_valid(run) || aligned.interval_count < 3 {
        return "insufficient_aligned_cycle_evidence";
    }
    if aligned.live_plateau {
        "available_passed"
    } else {
        "available_failed"
    }
}

fn rss_growth_acceptance_status(run: &ScenarioRun, growth: ResourceGrowthAnalysis) -> &'static str {
    match run.config.scenario {
        PerformanceScenario::FourBotStress => return "diagnostic_not_gated_for_scenario",
        PerformanceScenario::MapCycle100 => {
            return aligned_rss_growth_acceptance_status(run, aligned_cycle_growth_analysis(run));
        }
        PerformanceScenario::Soak10Minutes => {}
    }
    if !run.measurement_started {
        return "unavailable_measurement_not_started";
    }
    if run.measurement_elapsed < run.config.scenario.default_measurement_seconds() {
        return "insufficient_short_duration_evidence";
    }
    if run.process_memory_gib.is_empty() {
        return "unavailable_on_platform";
    }
    if !growth.resident_monotonic_growth && growth.final_resident_window_plateau {
        "passed"
    } else {
        "failed"
    }
}

fn live_growth_evidence_status(
    run: &ScenarioRun,
    growth: ResourceGrowthAnalysis,
    aligned: AlignedCycleGrowthAnalysis,
) -> &'static str {
    if run.config.scenario == PerformanceScenario::MapCycle100 {
        return aligned_live_growth_evidence_status(run, aligned);
    }
    if !run.measurement_started {
        "unavailable_measurement_not_started"
    } else if cfg!(feature = "perf-alloc") {
        if !growth.live_monotonic_growth && growth.final_live_window_plateau {
            "available_passed"
        } else {
            "available_failed"
        }
    } else {
        "unavailable_without_perf_alloc"
    }
}

fn requested_exit_code(fixture_valid: bool) -> u8 {
    if fixture_valid { 0 } else { 1 }
}

fn render_evidence(fence_valid: bool) -> &'static str {
    if fence_valid {
        "same_window_same_view_surface_texture_present_invoked"
    } else {
        "surface_present_invocation_not_observed"
    }
}

fn allocation_report_snapshot(measurement_started: bool) -> Option<AllocationSnapshot> {
    if !measurement_started {
        return None;
    }
    #[cfg(feature = "perf-alloc")]
    {
        return Some(end_allocation_measurement());
    }
    #[cfg(not(feature = "perf-alloc"))]
    None
}

fn report_result(run: &ScenarioRun, diagnostics: &DiagnosticsStore) {
    // `serde_json::json!` recursively parses object members and exceeds this
    // crate's recursion limit for the intentionally broad, flat report schema.
    // Expanding one value at a time keeps the wire shape flat without raising a
    // crate-wide compiler limit.
    macro_rules! flat_json_object {
        ($($key:literal : $value:expr),* $(,)?) => {{
            let mut object = serde_json::Map::new();
            $(
                object.insert($key.to_owned(), serde_json::json!($value));
            )*
            serde_json::Value::Object(object)
        }};
    }

    let allocations = allocation_report_snapshot(run.measurement_started);
    let allocation_count = allocations.map(|snapshot| {
        snapshot
            .allocation_count
            .saturating_sub(run.allocation_count_start)
    });
    let allocated_bytes = allocations.map(|snapshot| {
        snapshot
            .allocated_bytes
            .saturating_sub(run.allocated_bytes_start)
    });
    let live_bytes_start = allocations.map(|_| run.live_bytes_start);
    let live_bytes_end = allocations.map(|snapshot| snapshot.live_bytes);
    let live_bytes_delta = live_bytes_end
        .zip(live_bytes_start)
        .map(|(end, start)| i128::from(end) - i128::from(start));
    let peak_live_bytes = allocations.map(|snapshot| snapshot.peak_live_bytes);
    let allocation_measurement_status =
        allocation_measurement_status(run.measurement_started, cfg!(feature = "perf-alloc"));
    let frame = Distribution::from_samples(&run.frame_ms);
    let cpu_frame = Distribution::from_samples(&run.cpu_frame_ms);
    let frame_first_window = edge_window(&run.frame_ms, run.measurement_elapsed, false);
    let frame_last_window = edge_window(&run.frame_ms, run.measurement_elapsed, true);
    let cpu_first_window = edge_window(&run.cpu_frame_ms, run.measurement_elapsed, false);
    let cpu_last_window = edge_window(&run.cpu_frame_ms, run.measurement_elapsed, true);
    let process_cpu = Distribution::from_samples(&run.process_cpu_percent);
    let process_memory = Distribution::from_samples(&run.process_memory_gib);
    let process_memory_start = run.process_memory_gib.first().copied().unwrap_or_default();
    let process_memory_end = run.process_memory_gib.last().copied().unwrap_or_default();
    let switches = Distribution::from_samples(&run.switch_ms);
    let render_cpu_paths = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.path().as_str().starts_with("render/"))
        .filter(|diagnostic| diagnostic.path().as_str().ends_with("elapsed_cpu"))
        .count();
    let render_gpu_paths = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.path().as_str().starts_with("render/"))
        .filter(|diagnostic| diagnostic.path().as_str().ends_with("elapsed_gpu"))
        .count();
    let activity_valid = activity_valid(run);
    let owner_valid = owner_valid(run);
    let uses_four_bots = run.config.scenario.uses_four_bots();
    let (activity_total_floor, activity_hit_floor) = combat_activity_floors(run);
    let activity_owner_valid = uses_four_bots.then(|| combat_activity_owner_valid(run));
    let owner_workload_owner_valid = uses_four_bots.then(|| owner_workload_owner_valid(run));
    let activity_total_floor = uses_four_bots.then_some(activity_total_floor);
    let activity_hit_floor = uses_four_bots.then_some(activity_hit_floor);
    let journal_continuity_valid = journal_continuity_valid(run);
    let event_continuity_valid = event_continuity_valid(run);
    let fixture_valid = scenario_valid(run);
    let invalid_reasons = fixture_invalid_reasons(run);
    let primary_failure = fixture_failure(run);
    let requested_exit_code = requested_exit_code(fixture_valid);
    let capture_eligibility = CanonicalCaptureEligibility::from_config(&run.config);
    let canonical_capture_eligible = capture_eligibility.eligible();
    let fixed_combat_fixture = fixed_combat_fixture_arena(run.config.scenario);
    let fixed_combat_fixture_mode = if fixed_combat_fixture.is_some() {
        "fixed"
    } else {
        "nonfixed_map_cycle"
    };
    let growth = resource_growth_analysis(run);
    let aligned = aligned_cycle_growth_analysis(run);
    let resource_start = run
        .resource_start
        .or_else(|| run.resource_growth.first().map(|sample| sample.resources))
        .unwrap_or_default();
    let map_first_cycle = run.map_first_cycle_resources.unwrap_or_default();
    let map_switch_samples_valid = map_switch_samples_valid(run);
    let resource_stability_valid = resource_stability_valid(run);
    let rss_evidence_available = !run.process_memory_gib.is_empty();
    let frame_p99_budget_pass = frame.count > 0 && frame.p99 < FRAME_P99_BUDGET_MS;
    let cpu_mean_budget_pass = cpu_frame.count > 0 && cpu_frame.mean < CPU_MEAN_BUDGET_MS;
    let local_timing_budget_pass = frame_p99_budget_pass && cpu_mean_budget_pass;
    let rss_growth_acceptance_status = rss_growth_acceptance_status(run, growth);
    let aligned_rss_growth_acceptance_status = aligned_rss_growth_acceptance_status(run, aligned);
    let live_growth_evidence_status = live_growth_evidence_status(run, growth, aligned);
    let aligned_live_growth_evidence_status = aligned_live_growth_evidence_status(run, aligned);
    let timing_gate_pass = cfg!(feature = "perf-alloc") || local_timing_budget_pass;
    let rss_gate_pass = match run.config.scenario {
        PerformanceScenario::FourBotStress => true,
        PerformanceScenario::MapCycle100 => aligned_rss_growth_acceptance_status == "passed",
        PerformanceScenario::Soak10Minutes => rss_growth_acceptance_status == "passed",
    };
    let live_gate_pass = !cfg!(feature = "perf-alloc")
        || match run.config.scenario {
            PerformanceScenario::FourBotStress => true,
            PerformanceScenario::MapCycle100 => {
                aligned_live_growth_evidence_status == "available_passed"
            }
            PerformanceScenario::Soak10Minutes => live_growth_evidence_status == "available_passed",
        };
    let performance_acceptance_status = performance_acceptance_status(
        fixture_valid,
        canonical_capture_eligible,
        timing_gate_pass,
        rss_gate_pass,
        live_gate_pass,
    );
    let external_gpu_evidence_status = match performance_acceptance_status {
        "external_gpu_evidence_required" => "required_not_collected",
        "fixture_invalid" => "not_evaluated_fixture_invalid",
        "exploratory_only_noncanonical_configuration" => "not_evaluated_exploratory_only",
        "local_timing_budget_failed" => "not_evaluated_local_timing_budget_failed",
        "rss_growth_evidence_failed" => "not_evaluated_rss_growth_evidence_failed",
        "live_growth_evidence_failed" => "not_evaluated_live_growth_evidence_failed",
        _ => "not_evaluated_unknown_status",
    };
    let resource_growth_series = run
        .resource_growth
        .iter()
        .map(|sample| {
            serde_json::json!({
                "elapsed_seconds": sample.elapsed_seconds,
                "entities": sample.resources.entities,
                "meshes": sample.resources.meshes,
                "materials": sample.resources.materials,
                "images": sample.resources.images,
                "resident_gib": sample.resident_gib,
                "live_bytes": sample.live_bytes,
                "stale_owner_entities": sample.stale_owner_entities,
            })
        })
        .collect::<Vec<_>>();
    let process_memory_observations = run
        .process_memory_observations
        .iter()
        .map(|observation| {
            serde_json::json!({
                "elapsed_seconds": observation.elapsed_seconds,
                "resident_gib": observation.resident_gib,
            })
        })
        .collect::<Vec<_>>();
    let aligned_cycle_checkpoints = run
        .map_cycle_checkpoints
        .iter()
        .map(|checkpoint| {
            serde_json::json!({
                "cycle": checkpoint.cycle,
                "present_epoch": checkpoint.present_epoch,
                "elapsed_seconds": checkpoint.elapsed_seconds,
                "entities": checkpoint.resources.entities,
                "meshes": checkpoint.resources.meshes,
                "materials": checkpoint.resources.materials,
                "images": checkpoint.resources.images,
                "resident_gib": checkpoint.resident_gib,
                "live_bytes": checkpoint.live_bytes,
            })
        })
        .collect::<Vec<_>>();

    let result = flat_json_object! {
        "schema_version": PERFORMANCE_RESULT_SCHEMA_VERSION,
        "scenario": run.config.scenario.label(),
        "run_id": run.config.run_id.as_str(),
        "seed": format!("0x{:016x}", run.config.seed),
        "warmup_seconds": run.config.warmup_seconds,
        "requested_measurement_seconds": run.config.measurement_seconds,
        "measurement_seconds": run.measurement_elapsed,
        "canonical_warmup_eligible": capture_eligibility.warmup,
        "canonical_duration_eligible": capture_eligibility.duration,
        "canonical_seed_eligible": capture_eligibility.seed,
        "canonical_present_mode_eligible": capture_eligibility.present_mode,
        "canonical_capture_eligible": canonical_capture_eligible,
        "present_mode_policy": present_mode_policy(run.config.uncapped_present_mode),
        "uncapped_present_mode": run.config.uncapped_present_mode,
        "present_mode_policy_valid": run.present_mode_policy_valid,
        "continuous_update_mode_valid": run.continuous_update_mode_valid,
        "map_cycle_preload_required": run.config.scenario == PerformanceScenario::MapCycle100,
        "map_cycle_preload_ready": run.map_cycle_preload_ready,
        "map_cycle_preload_asset_count": run.map_cycle_preload_asset_count,
        "map_cycle_preload_folders": MAP_CYCLE_PRELOAD_FOLDERS,
        "requested_exit_code": requested_exit_code,
        "frame_samples": frame.count,
        "frame_timing_aggregate_authoritative": true,
        "frame_classification_sample_count": run.frame_classification.total(),
        "frame_classification_steady_samples": run.frame_classification.steady,
        "frame_classification_transition_samples": run.frame_classification.transition,
        "frame_classification_finalization_samples": run.frame_classification.finalization,
        "frame_classification_gap_valid": frame_classification_valid(run),
        "frame_mean_ms": frame.mean,
        "frame_median_ms": frame.median,
        "frame_p95_ms": frame.p95,
        "frame_p99_ms": frame.p99,
        "frame_max_ms": frame.max,
        "cpu_frame_mean_ms": cpu_frame.mean,
        "cpu_frame_median_ms": cpu_frame.median,
        "cpu_frame_p95_ms": cpu_frame.p95,
        "cpu_frame_p99_ms": cpu_frame.p99,
        "cpu_frame_max_ms": cpu_frame.max,
        "frame_first_window_p95_ms": frame_first_window.p95,
        "frame_last_window_p95_ms": frame_last_window.p95,
        "cpu_first_window_p95_ms": cpu_first_window.p95,
        "cpu_last_window_p95_ms": cpu_last_window.p95,
        "process_cpu_mean_percent": process_cpu.mean,
        "process_cpu_median_percent": process_cpu.median,
        "process_cpu_p95_percent": process_cpu.p95,
        "process_memory_start_gib": process_memory_start,
        "process_memory_median_gib": process_memory.median,
        "process_memory_peak_gib": process_memory.max,
        "process_memory_end_gib": process_memory_end,
        "process_memory_observation_interval_seconds": 1.0,
        "process_memory_observations": process_memory_observations,
        "allocations": allocation_count,
        "allocated_bytes": allocated_bytes,
        "live_bytes_start": live_bytes_start,
        "live_bytes_end": live_bytes_end,
        "live_bytes_delta": live_bytes_delta,
        "peak_live_bytes": peak_live_bytes,
        "allocation_measurement_status": allocation_measurement_status,
        "measurement_started": run.measurement_started,
        "entity_start": resource_start.entities,
        "entity_peak": run.entity_peak,
        "entity_end": run.entity_end,
        "mesh_start": resource_start.meshes,
        "mesh_peak": run.mesh_peak,
        "mesh_end": run.mesh_end,
        "material_start": resource_start.materials,
        "material_peak": run.material_peak,
        "material_end": run.material_end,
        "image_start": resource_start.images,
        "image_peak": run.image_peak,
        "image_end": run.image_end,
        "map_first_cycle_entities": map_first_cycle.entities,
        "map_first_cycle_meshes": map_first_cycle.meshes,
        "map_first_cycle_materials": map_first_cycle.materials,
        "map_first_cycle_images": map_first_cycle.images,
        "map_cycle_resource_checkpoint_observed": run.map_first_cycle_resources.is_some(),
        "map_warm_precycle_switches": run.map_warm_precycle_switches,
        "map_warm_precycle_present_ack_count": run.map_warm_precycle_present_ack_count,
        "map_warm_precycle_valid": map_warm_precycle_evidence_valid(run),
        "map_switches": run.map_switches,
        "map_switch_samples": run.switch_ms.len(),
        "map_switch_samples_valid": map_switch_samples_valid,
        "map_measured_present_ack_count": run.map_measured_present_ack_count,
        "initial_present_ack_count": run.initial_present_ack_count,
        "final_present_ack_count": run.final_present_ack_count,
        "present_ack_count": run.present_ack_count,
        "final_present_requested_epoch": run.final_present_requested_epoch,
        "final_present_observed_epoch": run.final_present_observed_epoch,
        "switch_mean_ms": switches.mean,
        "switch_median_ms": switches.median,
        "switch_p95_ms": switches.p95,
        "switch_p99_ms": switches.p99,
        "switch_max_ms": switches.max,
        "resource_growth_samples": growth.sample_count,
        "resource_growth_samples_observed": run.resource_growth.total_observed,
        "resource_growth_history_samples": run.resource_growth.history_len,
        "resource_growth_exact_tail_samples": run.resource_growth.tail_len,
        "resource_growth_thinning_count": run.resource_growth.thinning_count,
        "resource_growth_series": resource_growth_series,
        "aligned_cycle_checkpoint_count": aligned.checkpoint_count,
        "aligned_cycle_tail_checkpoint_count": aligned
            .checkpoint_count
            .min(MAP_ALIGNED_TAIL_CHECKPOINTS),
        "aligned_cycle_interval_count": aligned.interval_count,
        "aligned_cycle_span_seconds": aligned.span_seconds,
        "aligned_cycle_resource_counts_valid": aligned.resource_counts_valid,
        "aligned_cycle_checkpoints": aligned_cycle_checkpoints,
        "aligned_rss_tail_range_mib": aligned.resident_range_mib,
        "aligned_rss_tail_slope_mib_per_minute": aligned.resident_slope_mib_per_minute,
        "aligned_rss_tail_plateau": aligned.resident_plateau,
        "aligned_live_tail_range_mib": aligned.live_range_mib,
        "aligned_live_tail_slope_mib_per_minute": aligned.live_slope_mib_per_minute,
        "aligned_live_tail_plateau": aligned.live_plateau,
        "aligned_rss_growth_acceptance_status": aligned_rss_growth_acceptance_status,
        "aligned_live_growth_evidence_status": aligned_live_growth_evidence_status,
        "stale_owner_sample_peak": growth.stale_owner_sample_peak,
        "entity_slope_per_minute": growth.entity_slope_per_minute,
        "mesh_slope_per_minute": growth.mesh_slope_per_minute,
        "material_slope_per_minute": growth.material_slope_per_minute,
        "image_slope_per_minute": growth.image_slope_per_minute,
        "resident_mib_slope_per_minute": growth.resident_mib_slope_per_minute,
        "live_mib_slope_per_minute": growth.live_mib_slope_per_minute,
        "entity_monotonic_growth": growth.entity_monotonic_growth,
        "mesh_monotonic_growth": growth.mesh_monotonic_growth,
        "material_monotonic_growth": growth.material_monotonic_growth,
        "image_monotonic_growth": growth.image_monotonic_growth,
        "resident_monotonic_growth": growth.resident_monotonic_growth,
        "live_monotonic_growth": growth.live_monotonic_growth,
        "final_asset_window_plateau": growth.final_asset_window_plateau,
        "final_resident_window_range_mib": growth.final_resident_window_range_mib,
        "final_resident_window_slope_mib_per_minute": growth.final_resident_window_slope_mib_per_minute,
        "final_resident_window_plateau": growth.final_resident_window_plateau,
        "final_live_window_range_mib": growth.final_live_window_range_mib,
        "final_live_window_slope_mib_per_minute": growth.final_live_window_slope_mib_per_minute,
        "final_live_window_plateau": growth.final_live_window_plateau,
        "rss_plateau_range_budget_mib": RSS_PLATEAU_RANGE_MIB,
        "rss_plateau_slope_budget_mib_per_minute": RSS_PLATEAU_SLOPE_MIB_PER_MINUTE,
        "live_plateau_range_budget_mib": LIVE_BYTES_PLATEAU_RANGE_MIB,
        "live_plateau_slope_budget_mib_per_minute": LIVE_BYTES_PLATEAU_SLOPE_MIB_PER_MINUTE,
        "resource_stability_valid": resource_stability_valid,
        "rss_evidence_available": rss_evidence_available,
        "process_memory_samples": run.process_memory_gib.len(),
        "rss_growth_acceptance_status": rss_growth_acceptance_status,
        "live_growth_evidence_status": live_growth_evidence_status,
        "stale_owner_entities_peak": run.stale_owner_entities_peak,
        "stale_owner_entities_end": run.stale_owner_entities_end,
        "render_cpu_diagnostic_paths": render_cpu_paths,
        "render_gpu_diagnostic_paths": render_gpu_paths,
        "allocation_instrumentation": cfg!(feature = "perf-alloc"),
        "mode": "dev_perf_graphical",
        "fixture": run.config.scenario.label(),
        "fixed_combat_fixture_mode": fixed_combat_fixture_mode,
        "fixed_combat_fixture_arena_index": fixed_combat_fixture.map(|fixture| fixture.index),
        "fixed_combat_fixture_arena_name": fixed_combat_fixture.map(|fixture| fixture.name),
        "fixed_combat_fixture_authored_items": fixed_combat_fixture
            .map(|fixture| fixture.authored_items),
        "fixed_combat_fixture_authored_hazards": fixed_combat_fixture
            .map(|fixture| fixture.authored_hazards),
        "fixed_combat_fixture_public_hazard_markers": fixed_combat_fixture
            .map(|fixture| fixture.public_hazard_markers),
        "fixture_expected_fighters": run.fixture_expected_fighters,
        "fixture_observed_fighters": run.fixture_observed_fighters,
        "fixture_expected_combatant_bots": run.fixture_expected_combatant_bots,
        "fixture_observed_combatant_bots": run.fixture_observed_combatant_bots,
        "fixture_expected_items": run.fixture_expected_items,
        "fixture_observed_items": run.fixture_observed_items,
        "fixture_expected_hazard_markers": run.fixture_expected_hazard_markers,
        "fixture_observed_hazard_markers": run.fixture_observed_hazard_markers,
        "fixture_counts_valid": run.fixture_counts_valid,
        "bots_promoted_from_canonical_fixture": run.bots_promoted_from_canonical_fixture,
        "scene_root_count": run.scene_root_count,
        "ready_scene_instance_count": run.ready_scene_instance_count,
        "scene_root_readiness_non_vacuous": run.scene_root_readiness_non_vacuous,
        "scene_instances_ready": run.scene_instances_ready,
        "simulation_ready": run.readiness_valid,
        "surface_present_invocation_observed": run.fence_valid,
        "render_evidence": render_evidence(run.fence_valid),
        "gpu_completion_measured": false,
        "external_gpu_evidence_status": external_gpu_evidence_status,
        "frame_p99_budget_ms": FRAME_P99_BUDGET_MS,
        "frame_p99_budget_pass": frame_p99_budget_pass,
        "cpu_mean_budget_ms": CPU_MEAN_BUDGET_MS,
        "cpu_mean_budget_pass": cpu_mean_budget_pass,
        "local_timing_budget_pass": local_timing_budget_pass,
        "performance_acceptance_status": performance_acceptance_status,
        "combat_activity_total_floor_per_owner": activity_total_floor,
        "combat_hit_floor_per_owner": activity_hit_floor,
        "combat_activity_owner_valid": activity_owner_valid,
        "combat_actions": run.activity.map(|activity| activity.actions),
        "combat_hits": run.activity.map(|activity| activity.hits),
        "combat_guards": run.activity.map(|activity| activity.guards),
        "combat_items": run.activity.map(|activity| activity.items),
        "combat_abilities": run.activity.map(|activity| activity.abilities),
        "combat_activity_total": run.activity.map(OwnerActivity::total),
        "owner_workload_peak": run.owner_peak.map(OwnerWorkload::total),
        "owner_workload_end": run.owner_end.map(OwnerWorkload::total),
        "owner_workload_owner_valid": owner_workload_owner_valid,
        "journal_gap_ticks": run.journal_gap_ticks,
        "journal_continuity_valid": journal_continuity_valid,
        "event_overflow_delta": run
            .event_overflow_end
            .saturating_sub(run.event_overflow_start),
        "event_continuity_valid": event_continuity_valid,
        "activity_valid": activity_valid,
        "owner_valid": owner_valid,
        "fixture_valid": fixture_valid,
        "fixture_invalid_reasons": invalid_reasons,
        "failure": primary_failure,
    };
    info!("AFC_PERF_RESULT {result}");
}

fn edge_window(samples: &[f64], measurement_seconds: f64, from_end: bool) -> Distribution {
    if samples.is_empty() {
        return Distribution::default();
    }
    let fraction = (60.0 / measurement_seconds.max(60.0)).min(0.5);
    let count = ((samples.len() as f64 * fraction).round() as usize).clamp(1, samples.len());
    if from_end {
        Distribution::from_samples(&samples[samples.len() - count..])
    } else {
        Distribution::from_samples(&samples[..count])
    }
}

#[derive(Clone, Copy, Debug)]
struct ProcessSample {
    cpu_seconds: f64,
    resident_gib: f64,
}

#[cfg(unix)]
fn process_sample() -> Option<ProcessSample> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    let result = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    if result != 0 {
        return None;
    }
    let usage = unsafe { usage.assume_init() };
    let cpu_seconds = timeval_seconds(usage.ru_utime) + timeval_seconds(usage.ru_stime);
    process_resident_bytes().map(|resident_bytes| ProcessSample {
        cpu_seconds,
        resident_gib: resident_bytes as f64 / 1024.0 / 1024.0 / 1024.0,
    })
}

#[cfg(unix)]
fn timeval_seconds(value: libc::timeval) -> f64 {
    value.tv_sec as f64 + value.tv_usec as f64 / 1_000_000.0
}

#[cfg(target_os = "macos")]
#[allow(deprecated)]
fn process_resident_bytes() -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::mach_task_basic_info>::zeroed();
    let mut count = libc::MACH_TASK_BASIC_INFO_COUNT;
    let result = unsafe {
        libc::task_info(
            libc::mach_task_self(),
            libc::MACH_TASK_BASIC_INFO,
            info.as_mut_ptr().cast(),
            &mut count,
        )
    };
    if result != libc::KERN_SUCCESS {
        return None;
    }
    let info = unsafe { info.assume_init() };
    Some(info.resident_size as u64)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn process_resident_bytes() -> Option<u64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let resident_pages = statm.split_whitespace().nth(1)?.parse::<u64>().ok()?;
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    (page_size > 0).then_some(resident_pages.saturating_mul(page_size as u64))
}

#[cfg(not(unix))]
fn process_sample() -> Option<ProcessSample> {
    None
}

fn environment_flag(name: &str) -> bool {
    std::env::var(name).ok().is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

#[derive(Clone, Copy, Debug, Default)]
struct Distribution {
    count: usize,
    mean: f64,
    median: f64,
    p95: f64,
    p99: f64,
    max: f64,
}

impl Distribution {
    fn from_samples(samples: &[f64]) -> Self {
        if samples.is_empty() {
            return Self::default();
        }
        let mut sorted = samples.to_vec();
        sorted.sort_by(f64::total_cmp);
        Self {
            count: sorted.len(),
            mean: sorted.iter().sum::<f64>() / sorted.len() as f64,
            median: percentile(&sorted, 0.50),
            p95: percentile(&sorted, 0.95),
            p99: percentile(&sorted, 0.99),
            max: *sorted.last().unwrap_or(&0.0),
        }
    }
}

fn percentile(sorted: &[f64], percentile: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let index = ((sorted.len() - 1) as f64 * percentile).ceil() as usize;
    sorted[index.min(sorted.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenario_names_are_case_and_separator_insensitive() {
        assert_eq!(
            PerformanceScenario::parse("four-bot_stress"),
            Some(PerformanceScenario::FourBotStress)
        );
        assert_eq!(
            PerformanceScenario::parse("MapCycle100"),
            Some(PerformanceScenario::MapCycle100)
        );
        assert_eq!(
            PerformanceScenario::parse("soak-10-minutes"),
            Some(PerformanceScenario::Soak10Minutes)
        );
    }

    #[test]
    fn profiling_update_mode_remains_continuous_without_window_focus() {
        let settings = WinitSettings::continuous();
        assert!(continuous_update_mode_valid(&settings));
        assert!(!continuous_update_mode_valid(&WinitSettings::game()));
    }

    #[test]
    fn present_mode_policy_is_vsync_by_default_and_uncapped_only_by_opt_in() {
        assert_eq!(present_mode_policy(false), "AutoVsync");
        assert_eq!(expected_present_mode(false), PresentMode::AutoVsync);
        assert_eq!(present_mode_policy(true), "AutoNoVsync");
        assert_eq!(expected_present_mode(true), PresentMode::AutoNoVsync);
    }

    #[test]
    fn frame_buffers_are_pretouched_and_frame_classes_partition_exactly() {
        let buffer = pretouched_f64_buffer(8_193);
        assert!(buffer.is_empty());
        assert!(buffer.capacity() >= 8_193);

        let classes = FrameClassificationCounts {
            steady: 997,
            transition: 100,
            finalization: 3,
        };
        assert_eq!(classes.total(), 1_100);
        assert!(classes.valid_for(1_100));
        assert!(!classes.valid_for(1_099));
    }

    #[test]
    fn map_preload_status_waits_for_every_folder_and_fails_closed() {
        assert_eq!(
            combine_asset_preload_statuses(std::iter::empty::<AssetPreloadStatus>()),
            AssetPreloadStatus::Pending
        );
        assert_eq!(
            combine_asset_preload_statuses([
                AssetPreloadStatus::Ready,
                AssetPreloadStatus::Pending,
            ]),
            AssetPreloadStatus::Pending
        );
        assert_eq!(
            combine_asset_preload_statuses(
                [AssetPreloadStatus::Failed, AssetPreloadStatus::Ready,]
            ),
            AssetPreloadStatus::Failed
        );
        assert_eq!(
            combine_asset_preload_statuses([AssetPreloadStatus::Ready, AssetPreloadStatus::Ready,]),
            AssetPreloadStatus::Ready
        );
        assert_eq!(
            MAP_CYCLE_PRELOAD_FOLDERS,
            ["arena", "backgrounds", "music/bgm"]
        );

        let paths = discover_map_cycle_asset_paths_from_root(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("assets"),
        )
        .expect("checked-in render assets must be discoverable");
        assert_eq!(paths.len(), 101);
        assert!(paths.contains(&"backgrounds/crown_ring.png".to_string()));
        assert!(paths.contains(&"arena/kits/platformer/lever.glb".to_string()));
        assert!(paths.iter().any(|path| path.ends_with(".ogg")));
        assert!(paths.iter().all(|path| {
            path.ends_with(".glb")
                || path.ends_with(".png")
                || path.ends_with(".ogg")
                || path.ends_with(".mp3")
                || path.ends_with(".wav")
        }));
        assert!(paths.iter().all(|path| !path.ends_with("License.txt")));
    }

    #[test]
    fn map_measurement_waits_for_an_exact_completed_warm_cycle() {
        assert_eq!(
            arena_definitions().len(),
            MAP_WARM_PRECYCLE_SWITCH_COUNT,
            "one warm precycle must remain exactly one normal arena rotation"
        );
        let mut run = ScenarioRun::new(ScenarioConfig {
            scenario: PerformanceScenario::MapCycle100,
            warmup_seconds: 0.0,
            measurement_seconds: 1.0,
            seed: 1,
            run_id: "warm-precycle".to_string(),
            uncapped_present_mode: false,
        });
        run.phase = MeasurementPhase::Warmup;
        assert!(!warmup_fixture_ready(&run));

        run.map_warm_precycle_switches = MAP_WARM_PRECYCLE_SWITCH_COUNT;
        run.map_warm_precycle_present_ack_count = MAP_WARM_PRECYCLE_SWITCH_COUNT;
        run.map_warm_precycle_valid = true;
        run.pending_present_epoch = Some(11);
        assert!(!warmup_fixture_ready(&run));

        run.pending_present_epoch = None;
        run.pending_switch_started = Some(Instant::now());
        assert!(!warmup_fixture_ready(&run));

        run.pending_switch_started = None;
        assert!(warmup_fixture_ready(&run));
        assert!(map_warm_precycle_evidence_valid(&run));
    }

    #[test]
    fn seed_parser_accepts_decimal_and_hexadecimal() {
        assert_eq!(parse_seed("42"), Some(42));
        assert_eq!(parse_seed("0x2a"), Some(42));
        assert_eq!(parse_seed("not-a-seed"), None);
    }

    #[test]
    fn distribution_reports_nearest_rank_percentiles() {
        let distribution = Distribution::from_samples(&[4.0, 1.0, 3.0, 2.0]);
        assert_eq!(distribution.count, 4);
        assert_eq!(distribution.mean, 2.5);
        assert_eq!(distribution.median, 3.0);
        assert_eq!(distribution.p95, 4.0);
        assert_eq!(distribution.p99, 4.0);
        assert_eq!(distribution.max, 4.0);
    }

    #[test]
    fn edge_windows_select_the_first_and_last_minute_fraction() {
        let samples = (1..=10).map(f64::from).collect::<Vec<_>>();
        let first = edge_window(&samples, 300.0, false);
        let last = edge_window(&samples, 300.0, true);
        assert_eq!(first.count, 2);
        assert_eq!(first.max, 2.0);
        assert_eq!(last.count, 2);
        assert_eq!(last.median, 10.0);
    }

    #[test]
    fn surface_present_fence_epoch_is_monotonic_and_extractable() {
        let mut fence = SurfacePresentFence::default();
        assert_eq!(fence.request(), 1);
        let extracted = SurfacePresentFence::extract_resource(&fence);
        assert_eq!(extracted.requested_epoch, 1);
        assert_eq!(fence.request(), 2);
        assert_eq!(fence.requested_epoch, 2);
    }

    #[test]
    fn present_acknowledgement_requires_written_target_and_exact_consumed_surface() {
        assert!(present_invocation_observed(7, true, true));
        assert!(!present_invocation_observed(0, true, true));
        assert!(!present_invocation_observed(7, false, true));
        assert!(!present_invocation_observed(7, true, false));
    }

    #[test]
    fn present_trace_is_disabled_by_default_and_bounded_per_epoch() {
        let mut probe = SurfacePresentProbe::default();
        assert!(!probe.take_arm_trace_sample(false, 7));
        assert!(!probe.take_arm_trace_sample(true, 0));
        for _ in 0..4 {
            assert!(probe.take_arm_trace_sample(true, 7));
            assert!(probe.take_post_trace_sample(true, 7));
        }
        assert!(!probe.take_arm_trace_sample(true, 7));
        assert!(!probe.take_post_trace_sample(true, 7));
        assert!(probe.take_arm_trace_sample(true, 8));
        assert!(probe.take_post_trace_sample(true, 8));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn terminal_exit_is_dispatched_on_the_frame_after_last_records_it() {
        #[derive(Resource, Default)]
        struct LastExitObservation(Option<AppExit>);

        fn request_failure_once(mut run: ResMut<ScenarioRun>) {
            if run.phase != MeasurementPhase::Failed {
                run.phase = MeasurementPhase::Failed;
                run.request_exit(AppExit::error());
            }
        }

        fn observe_exit_in_last(
            mut exits: MessageReader<AppExit>,
            mut observation: ResMut<LastExitObservation>,
        ) {
            observation.0 = exits.read().next().cloned();
        }

        let mut app = App::new();
        app.add_message::<AppExit>()
            .init_resource::<LastExitObservation>()
            .insert_resource(ScenarioRun::new(ScenarioConfig {
                scenario: PerformanceScenario::FourBotStress,
                warmup_seconds: 0.0,
                measurement_seconds: 0.1,
                seed: 7,
                run_id: "deferred-exit-test".to_string(),
                uncapped_present_mode: false,
            }))
            .add_systems(PreUpdate, dispatch_performance_exit)
            .add_systems(Last, (request_failure_once, observe_exit_in_last));

        app.update();
        assert_eq!(app.should_exit(), None);
        assert_eq!(app.world().resource::<LastExitObservation>().0, None);
        app.update();
        assert_eq!(app.should_exit(), Some(AppExit::error()));
        assert_eq!(
            app.world().resource::<LastExitObservation>().0,
            Some(AppExit::error())
        );
        assert!(app.world().resource::<ScenarioRun>().pending_exit.is_none());
    }

    #[cfg(target_os = "macos")]
    fn assert_window_first_terminal_exit(status: AppExit) {
        #[derive(Resource)]
        struct RequestedTerminalStatus(AppExit);

        #[derive(Resource, Default)]
        struct TerminalRequestSent(bool);

        #[derive(Resource, Default)]
        struct LastShutdownObservation {
            exit: Option<AppExit>,
            removed_windows: usize,
        }

        fn request_terminal_once(
            requested: Res<RequestedTerminalStatus>,
            mut sent: ResMut<TerminalRequestSent>,
            mut run: ResMut<ScenarioRun>,
        ) {
            if !sent.0 {
                run.request_exit(requested.0.clone());
                sent.0 = true;
            }
        }

        fn observe_shutdown_in_last(
            mut exits: MessageReader<AppExit>,
            mut removed_windows: RemovedComponents<Window>,
            mut observation: ResMut<LastShutdownObservation>,
        ) {
            observation.exit = exits.read().next().cloned();
            observation.removed_windows += removed_windows.read().count();
        }

        let mut app = App::new();
        let window = app.world_mut().spawn(Window::default()).id();
        app.add_message::<AppExit>()
            .insert_resource(RequestedTerminalStatus(status.clone()))
            .init_resource::<TerminalRequestSent>()
            .init_resource::<LastShutdownObservation>()
            .insert_resource(ScenarioRun::new(ScenarioConfig {
                scenario: PerformanceScenario::FourBotStress,
                warmup_seconds: 0.0,
                measurement_seconds: 0.1,
                seed: 7,
                run_id: "window-first-exit-test".to_string(),
                uncapped_present_mode: false,
            }))
            .add_systems(PreUpdate, dispatch_performance_exit)
            .add_systems(
                Last,
                (request_terminal_once, observe_shutdown_in_last).chain(),
            );

        // N: Last records the terminal result without exposing AppExit yet.
        app.update();
        assert_eq!(app.should_exit(), None);
        assert!(app.world().get::<Window>(window).is_some());
        assert_eq!(
            app.world().resource::<ScenarioRun>().pending_exit.as_ref(),
            Some(&status)
        );
        assert_eq!(app.world().resource::<LastShutdownObservation>().exit, None);
        assert_eq!(
            app.world()
                .resource::<LastShutdownObservation>()
                .removed_windows,
            0
        );

        // N+1: PreUpdate despawns the window; Last witnesses its removal.
        app.update();
        assert_eq!(app.should_exit(), None);
        assert!(app.world().get::<Window>(window).is_none());
        assert_eq!(
            app.world().resource::<ScenarioRun>().pending_exit.as_ref(),
            Some(&status)
        );
        assert_eq!(app.world().resource::<LastShutdownObservation>().exit, None);
        assert_eq!(
            app.world()
                .resource::<LastShutdownObservation>()
                .removed_windows,
            1
        );

        // N+2: the exact stored status is finally published.
        app.update();
        assert_eq!(app.should_exit(), Some(status.clone()));
        assert_eq!(
            app.world().resource::<LastShutdownObservation>().exit,
            Some(status)
        );
        assert_eq!(
            app.world()
                .resource::<LastShutdownObservation>()
                .removed_windows,
            1
        );
        assert!(app.world().resource::<ScenarioRun>().pending_exit.is_none());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn terminal_success_waits_for_window_teardown_and_drain_update() {
        assert_window_first_terminal_exit(AppExit::Success);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn terminal_error_preserves_exact_status_after_window_teardown() {
        assert_window_first_terminal_exit(AppExit::from_code(7));
    }

    #[test]
    fn present_fixture_evidence_refuses_mismatched_epoch_scene_or_count() {
        let exact = PresentFixtureEvidence {
            epoch: 7,
            scene_identity: Some((3, 41)),
            expected_fighters: 4,
            observed_fighters: 4,
            expected_combatant_bots: 4,
            observed_combatant_bots: 4,
            expected_items: 6,
            observed_items: 6,
            expected_hazard_markers: 1,
            observed_hazard_markers: 1,
            scene_root_count: 1,
            ready_scene_instance_count: 1,
            scene_root_readiness_non_vacuous: true,
            scene_instances_ready: true,
            valid: true,
        };
        assert!(exact.allows_render_arm(7));
        assert!(!exact.allows_render_arm(6));
        assert!(!exact.proves(7, Some((3, 42))));

        let mismatched_count = PresentFixtureEvidence {
            observed_items: 5,
            ..exact
        };
        assert!(!mismatched_count.allows_render_arm(7));
        let invalid = PresentFixtureEvidence {
            valid: false,
            ..exact
        };
        assert!(!invalid.allows_render_arm(7));
        let incomplete_scene = PresentFixtureEvidence {
            scene_instances_ready: false,
            ..exact
        };
        assert!(!incomplete_scene.allows_render_arm(7));
        let vacuous_scene = PresentFixtureEvidence {
            scene_root_count: 0,
            ready_scene_instance_count: 0,
            scene_root_readiness_non_vacuous: false,
            ..exact
        };
        assert!(!vacuous_scene.allows_render_arm(7));
    }

    #[test]
    fn failed_readiness_reports_unavailable_measurement_and_fixture_first_statuses() {
        assert!(allocation_report_snapshot(false).is_none());
        assert_eq!(
            allocation_measurement_status(false, true),
            "unavailable_measurement_not_started"
        );
        assert_eq!(
            allocation_measurement_status(false, false),
            "unavailable_measurement_not_started"
        );
        assert_eq!(
            allocation_measurement_status(true, false),
            "unavailable_without_perf_alloc"
        );
        assert_eq!(allocation_measurement_status(true, true), "available");

        assert_eq!(
            performance_acceptance_status(false, true, true, true, true),
            "fixture_invalid"
        );
        assert_eq!(
            performance_acceptance_status(false, false, false, false, false),
            "fixture_invalid"
        );
        assert_eq!(
            performance_acceptance_status(true, true, false, true, true),
            "local_timing_budget_failed"
        );
        assert_eq!(
            performance_acceptance_status(true, true, true, false, true),
            "rss_growth_evidence_failed"
        );
        assert_eq!(
            performance_acceptance_status(true, true, true, true, false),
            "live_growth_evidence_failed"
        );
        assert_eq!(
            performance_acceptance_status(true, true, true, true, true),
            "external_gpu_evidence_required"
        );
        assert_eq!(
            render_evidence(false),
            "surface_present_invocation_not_observed"
        );
        assert_eq!(
            render_evidence(true),
            "same_window_same_view_surface_texture_present_invoked"
        );
    }

    #[test]
    fn result_contract_identifies_fixed_bumper_fixture_and_nonfixed_map_cycle() {
        assert_eq!(PERFORMANCE_RESULT_SCHEMA_VERSION, 6);

        let stress = fixed_combat_fixture_arena(PerformanceScenario::FourBotStress)
            .expect("FourBotStress uses one fixed combat arena");
        assert_eq!(stress.index, BUMPER_ALLEY_ARENA_INDEX);
        assert_eq!(stress.name, "Bumper Alley");
        assert_eq!(stress.authored_items, 4);
        assert_eq!(stress.authored_hazards, 3);
        assert_eq!(stress.public_hazard_markers, 3);
        assert_eq!(
            fixed_combat_fixture_arena(PerformanceScenario::Soak10Minutes),
            Some(stress)
        );
        assert_eq!(
            fixed_combat_fixture_arena(PerformanceScenario::MapCycle100),
            None
        );
    }

    #[test]
    fn readiness_counts_active_fighters_without_requiring_bot_brains() {
        let mut state = MatchState::default();
        state.set_active_slots([true, true, false, false]);
        let fighters = [
            Fighter {
                id: 0,
                name: "Human",
                color: Color::WHITE,
                spawn: Vec3::ZERO,
            },
            Fighter {
                id: 1,
                name: "Bot",
                color: Color::WHITE,
                spawn: Vec3::ZERO,
            },
            Fighter {
                id: 2,
                name: "Closed",
                color: Color::WHITE,
                spawn: Vec3::ZERO,
            },
        ];

        assert_eq!(observed_active_fighter_count(&state, &fighters), 2);
    }

    #[test]
    fn present_fixture_evidence_counts_a_human_without_a_bot_brain() {
        let mut run = ScenarioRun::new(ScenarioConfig {
            scenario: PerformanceScenario::MapCycle100,
            warmup_seconds: 0.0,
            measurement_seconds: 1.0,
            seed: 1,
            run_id: "human-and-bot".to_string(),
            uncapped_present_mode: false,
        });
        run.pending_present_epoch = Some(7);
        run.fixture_expected_fighters = 2;
        run.fixture_expected_combatant_bots = 0;

        let mut state = MatchState::default();
        state.set_active_slots([true, true, false, false]);

        let mut app = App::new();
        app.insert_resource(run);
        app.insert_resource(state);
        app.init_resource::<SceneSpawner>();
        app.init_resource::<PresentFixtureEvidence>();
        app.add_systems(Update, publish_present_fixture_evidence);
        app.world_mut().spawn((
            Fighter {
                id: 0,
                name: "Human",
                color: Color::WHITE,
                spawn: Vec3::ZERO,
            },
            Controller::new(
                crate::components::PlayerSlotId::new(0).unwrap(),
                ParticipantKind::Human,
                LocalInputAssignment::Keyboard(0),
            ),
        ));
        app.world_mut().spawn((
            Fighter {
                id: 1,
                name: "Bot",
                color: Color::WHITE,
                spawn: Vec3::ZERO,
            },
            Controller::new(
                crate::components::PlayerSlotId::new(1).unwrap(),
                ParticipantKind::Bot,
                LocalInputAssignment::Unassigned,
            ),
            crate::bot::default_bot_brain_for_fighter(1),
        ));

        app.update();

        let evidence = app.world().resource::<PresentFixtureEvidence>();
        assert_eq!(evidence.observed_fighters, 2);
        assert_eq!(evidence.observed_combatant_bots, 0);
    }

    #[test]
    fn result_contract_marks_only_default_warmup_duration_and_seed_as_canonical() {
        let canonical = ScenarioConfig {
            scenario: PerformanceScenario::FourBotStress,
            warmup_seconds: DEFAULT_WARMUP_SECONDS,
            measurement_seconds: DEFAULT_STRESS_SECONDS,
            seed: DEFAULT_REPLAY_SEED,
            run_id: "canonical".to_string(),
            uncapped_present_mode: false,
        };
        let eligibility = CanonicalCaptureEligibility::from_config(&canonical);
        assert_eq!(
            eligibility,
            CanonicalCaptureEligibility {
                warmup: true,
                duration: true,
                seed: true,
                present_mode: true,
            }
        );
        assert!(eligibility.eligible());

        for noncanonical in [
            ScenarioConfig {
                warmup_seconds: 5.0,
                ..canonical.clone()
            },
            ScenarioConfig {
                measurement_seconds: 60.0,
                ..canonical.clone()
            },
            ScenarioConfig {
                seed: DEFAULT_REPLAY_SEED.wrapping_add(1),
                ..canonical.clone()
            },
            ScenarioConfig {
                uncapped_present_mode: true,
                ..canonical.clone()
            },
        ] {
            assert!(!CanonicalCaptureEligibility::from_config(&noncanonical).eligible());
        }

        assert_eq!(
            performance_acceptance_status(true, false, true, true, true),
            "exploratory_only_noncanonical_configuration"
        );
        assert_eq!(requested_exit_code(true), 0);
        assert_eq!(requested_exit_code(false), 1);
    }

    #[test]
    fn result_contract_exposes_per_owner_floors_workload_and_continuity() {
        let mut run = ScenarioRun::new(ScenarioConfig {
            scenario: PerformanceScenario::FourBotStress,
            warmup_seconds: 5.0,
            measurement_seconds: 60.0,
            seed: DEFAULT_REPLAY_SEED,
            run_id: "diagnostic".to_string(),
            uncapped_present_mode: false,
        });
        run.activity[0].actions = 49;
        run.owner_peak[0].hitboxes = 1;

        assert_eq!(combat_activity_floors(&run), (2, 1));
        assert_eq!(combat_activity_owner_valid(&run), [false; 4]);
        assert_eq!(
            owner_workload_owner_valid(&run),
            [true, false, false, false]
        );
        assert!(journal_continuity_valid(&run));
        assert!(event_continuity_valid(&run));

        run.activity[0].hits = 1;
        assert_eq!(
            combat_activity_owner_valid(&run),
            [true, false, false, false]
        );

        run.activity = [OwnerActivity {
            actions: 1,
            hits: 1,
            ..default()
        }; 4];
        run.owner_peak = [OwnerWorkload {
            hitboxes: 1,
            ..default()
        }; 4];
        assert!(activity_valid(&run));
        assert!(owner_valid(&run));

        run.journal_gap_ticks = 1;
        run.event_overflow_end = 2;
        assert!(!journal_continuity_valid(&run));
        assert!(!event_continuity_valid(&run));
        assert_eq!(combat_activity_owner_valid(&run), [true; 4]);
        assert!(!activity_valid(&run));
    }

    #[test]
    fn result_contract_lists_all_invalid_gates_in_primary_failure_order() {
        let mut run = ScenarioRun::new(ScenarioConfig {
            scenario: PerformanceScenario::FourBotStress,
            warmup_seconds: DEFAULT_WARMUP_SECONDS,
            measurement_seconds: DEFAULT_STRESS_SECONDS,
            seed: DEFAULT_REPLAY_SEED,
            run_id: "invalid-reasons".to_string(),
            uncapped_present_mode: false,
        });
        run.measurement_started = true;
        run.continuous_update_mode_valid = true;
        run.present_mode_policy_valid = true;
        run.readiness_valid = true;
        run.fence_valid = true;
        run.scene_root_readiness_non_vacuous = true;
        run.scene_instances_ready = true;
        run.fixture_counts_valid = true;
        run.bots_promoted_from_canonical_fixture = true;
        run.initial_present_ack_count = 1;
        run.final_present_ack_count = 1;
        run.present_ack_count = 2;
        run.final_present_requested_epoch = 2;
        run.final_present_observed_epoch = 2;
        run.activity[0] = OwnerActivity {
            actions: 10,
            hits: 3,
            ..default()
        };
        run.owner_peak[0].hitboxes = 1;

        assert_eq!(
            fixture_invalid_reasons(&run),
            vec!["combat_activity_invalid", "owner_workload_invalid"]
        );
        assert_eq!(fixture_failure(&run), "combat_activity_invalid");
    }

    #[test]
    fn result_contract_qualifies_rss_by_scenario_and_completed_duration() {
        let mut stress = ScenarioRun::new(ScenarioConfig {
            scenario: PerformanceScenario::FourBotStress,
            warmup_seconds: 5.0,
            measurement_seconds: 60.0,
            seed: DEFAULT_REPLAY_SEED,
            run_id: "stress".to_string(),
            uncapped_present_mode: false,
        });
        stress.measurement_started = true;
        stress.measurement_elapsed = 60.0;
        assert_eq!(
            rss_growth_acceptance_status(&stress, resource_growth_analysis(&stress)),
            "diagnostic_not_gated_for_scenario"
        );

        let mut soak = ScenarioRun::new(ScenarioConfig {
            scenario: PerformanceScenario::Soak10Minutes,
            warmup_seconds: 5.0,
            measurement_seconds: 60.0,
            seed: DEFAULT_REPLAY_SEED,
            run_id: "short-soak".to_string(),
            uncapped_present_mode: false,
        });
        soak.measurement_started = true;
        soak.measurement_elapsed = 60.0;
        assert_eq!(
            rss_growth_acceptance_status(&soak, resource_growth_analysis(&soak)),
            "insufficient_short_duration_evidence"
        );

        soak.config.warmup_seconds = DEFAULT_WARMUP_SECONDS;
        soak.config.measurement_seconds = DEFAULT_SOAK_SECONDS;
        soak.measurement_elapsed = DEFAULT_SOAK_SECONDS;
        assert_eq!(
            rss_growth_acceptance_status(&soak, resource_growth_analysis(&soak)),
            "unavailable_on_platform"
        );

        soak.process_memory_gib.extend([0.5, 0.5]);
        for elapsed_seconds in [540.0, 600.0] {
            soak.resource_growth.push(ResourceGrowthSample {
                elapsed_seconds,
                resources: ResourceCounts {
                    entities: 100,
                    meshes: 10,
                    materials: 10,
                    images: 10,
                },
                resident_gib: 0.5,
                ..default()
            });
        }
        assert_eq!(
            rss_growth_acceptance_status(&soak, resource_growth_analysis(&soak)),
            "passed"
        );
    }

    #[test]
    fn hazard_marker_expectation_matches_specialized_visual_exclusions() {
        let crank = &arena_definitions()[CRANK_YARD_ARENA_INDEX];
        assert!(
            expected_hazard_marker_count(CRANK_YARD_ARENA_INDEX) < crank.hazards.len(),
            "Crank Yard saw blades use their specialized visual component"
        );
        let vent = &arena_definitions()[VENT_SPIRAL_ARENA_INDEX];
        assert_eq!(vent.name, "Vent Spiral");
        assert!(
            expected_hazard_marker_count(VENT_SPIRAL_ARENA_INDEX) < vent.hazards.len(),
            "Vent Spiral pulse vents use their specialized reactor components"
        );
    }

    #[test]
    fn allocation_end_waits_for_delayed_peak_publication_before_snapshot() {
        let control = AtomicU64::new(PEAK_PHASE_ACTIVE);
        let in_flight = AtomicU64::new(0);
        let measured_peak = AtomicU64::new(100);
        let live = AtomicU64::new(100);

        // Allocator A enters the active generation and raises LIVE to 200, then
        // stalls before publishing its local high-water mark.
        let contributes = enter_allocation_mutation(&control, &in_flight);
        assert!(contributes);
        let raised_live = live.fetch_add(100, Ordering::SeqCst) + 100;

        // End closes the generation. A concurrent free lowers LIVE, but the
        // boundary cannot finish while allocator A remains registered.
        let closing =
            enter_peak_measurement_transition(&control, PEAK_PHASE_ACTIVE, PEAK_PHASE_CLOSING);
        assert!(!peak_mutations_drained(&in_flight));
        live.store(100, Ordering::SeqCst);

        // A resumes, publishes 200 unconditionally from its captured ticket,
        // and releases. Only now may end read peak=200/live=100.
        publish_measured_peak(contributes, &measured_peak, raised_live);
        leave_allocation_mutation(&in_flight);
        assert!(peak_mutations_drained(&in_flight));
        assert_eq!(measured_peak.load(Ordering::SeqCst), 200);
        assert_eq!(live.load(Ordering::SeqCst), 100);
        control.store(closing + 1, Ordering::Release);
        assert_eq!(
            control.load(Ordering::Acquire) & PEAK_PHASE_MASK,
            PEAK_PHASE_INACTIVE
        );
    }

    #[test]
    fn measurement_activity_starts_after_the_warmup_journal_boundary() {
        let mut run = ScenarioRun::new(ScenarioConfig {
            scenario: PerformanceScenario::FourBotStress,
            warmup_seconds: 30.0,
            measurement_seconds: 300.0,
            seed: 1,
            run_id: "test".to_string(),
            uncapped_present_mode: false,
        });
        let boundary = crate::simulation::SimTick(417);
        run.begin_measurement(9, Some(boundary));

        assert_eq!(run.last_activity_tick, Some(boundary));
        assert_eq!(run.event_overflow_start, 9);
        assert_eq!(run.event_overflow_end, 9);
        assert_eq!(run.phase, MeasurementPhase::Measure);
        assert!(run.measurement_started);
    }

    #[test]
    fn periodic_process_observations_retain_the_exact_measurement_timestamp() {
        let mut run = ScenarioRun::new(ScenarioConfig {
            scenario: PerformanceScenario::FourBotStress,
            warmup_seconds: 1.0,
            measurement_seconds: 1.0,
            seed: 1,
            run_id: "process-observation".to_string(),
            uncapped_present_mode: false,
        });
        run.measurement_elapsed = 0.125;
        record_periodic_process_sample(
            &mut run,
            ProcessSample {
                cpu_seconds: 10.0,
                resident_gib: 0.5,
            },
        );
        run.measurement_elapsed = 1.25;
        record_periodic_process_sample(
            &mut run,
            ProcessSample {
                cpu_seconds: 10.5,
                resident_gib: 0.6,
            },
        );

        assert_eq!(run.process_memory_observations.len(), 2);
        assert_eq!(run.process_memory_observations[0].elapsed_seconds, 0.125);
        assert_eq!(run.process_memory_observations[1].elapsed_seconds, 1.25);
        assert_eq!(run.process_memory_gib, [0.5, 0.6]);
        assert_eq!(run.process_cpu_percent.len(), 1);
    }

    #[test]
    fn combat_activity_gate_is_only_applied_to_four_bot_fixtures() {
        let map = ScenarioRun::new(ScenarioConfig {
            scenario: PerformanceScenario::MapCycle100,
            warmup_seconds: 0.0,
            measurement_seconds: 1.0,
            seed: 1,
            run_id: "test".to_string(),
            uncapped_present_mode: false,
        });
        assert!(activity_valid(&map));
        assert!(owner_valid(&map));

        let mut stress = ScenarioRun::new(ScenarioConfig {
            scenario: PerformanceScenario::FourBotStress,
            ..map.config.clone()
        });
        stress.activity = [OwnerActivity {
            actions: 1,
            hits: 1,
            ..default()
        }; 4];
        stress.owner_peak = [OwnerWorkload {
            hitboxes: 1,
            ..default()
        }; 4];
        assert!(activity_valid(&stress));
        assert!(owner_valid(&stress));
    }

    #[test]
    fn resource_growth_history_is_bounded_and_retains_exact_late_samples() {
        let mut growth = BoundedResourceGrowth::default();
        for index in 0..(MAX_RESOURCE_GROWTH_SAMPLES + 8) {
            growth.push(ResourceGrowthSample {
                elapsed_seconds: index as f64,
                resources: ResourceCounts {
                    entities: index,
                    ..default()
                },
                ..default()
            });
        }
        assert_eq!(growth.len(), MAX_RESOURCE_GROWTH_SAMPLES);
        assert_eq!(growth.first().unwrap().resources.entities, 0);
        assert_eq!(
            growth.last().unwrap().resources.entities,
            MAX_RESOURCE_GROWTH_SAMPLES + 7
        );
        assert_eq!(
            growth.tail[..growth.tail_len]
                .iter()
                .flatten()
                .map(|sample| sample.resources.entities)
                .collect::<Vec<_>>(),
            (MAX_RESOURCE_GROWTH_SAMPLES - 8..MAX_RESOURCE_GROWTH_SAMPLES + 8).collect::<Vec<_>>()
        );
    }

    #[test]
    fn resource_growth_replaces_equal_time_sample_without_losing_raw_evidence() {
        let mut growth = BoundedResourceGrowth::default();
        growth.push(ResourceGrowthSample {
            elapsed_seconds: 5.0,
            resources: ResourceCounts {
                entities: 10,
                ..default()
            },
            ..default()
        });
        growth.push(ResourceGrowthSample {
            elapsed_seconds: 5.0,
            resources: ResourceCounts {
                entities: 8,
                ..default()
            },
            ..default()
        });

        assert_eq!(growth.len(), 1);
        assert_eq!(growth.total_observed, 2);
        assert_eq!(growth.last().unwrap().resources.entities, 8);
        assert!(
            !growth.monotonic_growth(0),
            "all raw observations must contribute to monotonic-growth evidence"
        );
    }

    #[test]
    fn resource_growth_thinning_preserves_whole_run_quartiles_and_exact_tail() {
        let mut growth = BoundedResourceGrowth::default();
        for index in 0..10_000 {
            growth.push(ResourceGrowthSample {
                elapsed_seconds: index as f64,
                resources: ResourceCounts {
                    entities: index,
                    ..default()
                },
                ..default()
            });
        }

        assert_eq!(growth.len(), MAX_RESOURCE_GROWTH_SAMPLES);
        assert_eq!(growth.total_observed, 10_000);
        assert!(growth.thinning_count > 0);
        assert_eq!(growth.first().unwrap().elapsed_seconds, 0.0);
        assert_eq!(growth.last().unwrap().elapsed_seconds, 9_999.0);
        let retained = growth
            .iter()
            .map(|sample| sample.elapsed_seconds as usize)
            .collect::<Vec<_>>();
        assert!(retained.windows(2).all(|pair| pair[0] < pair[1]));
        for quartile in 0..4 {
            assert!(
                growth.history[..growth.history_len]
                    .iter()
                    .flatten()
                    .any(|sample| {
                        let value = sample.elapsed_seconds as usize;
                        value >= quartile * 2_500 && value < (quartile + 1) * 2_500
                    }),
                "missing historical coverage for quartile {quartile}"
            );
        }
        assert_eq!(
            growth.tail[..growth.tail_len]
                .iter()
                .flatten()
                .map(|sample| sample.elapsed_seconds as usize)
                .collect::<Vec<_>>(),
            (9_984..10_000).collect::<Vec<_>>()
        );
    }

    #[test]
    fn resource_growth_overrun_keeps_late_intermediates_for_plateau_analysis() {
        let mut growth = BoundedResourceGrowth::default();
        for index in 0..200 {
            growth.push(ResourceGrowthSample {
                elapsed_seconds: index as f64 * 5.0,
                resources: ResourceCounts {
                    entities: if index == 100 { 1 } else { index },
                    ..default()
                },
                resident_gib: index as f64 / 1024.0,
                live_bytes: index as u64 * 1024 * 1024,
                stale_owner_entities: 0,
            });
        }
        let (count, range, slope) =
            final_window_stats(&growth, RESOURCE_PLATEAU_WINDOW_SECONDS, |sample| {
                sample.live_bytes as f64 / 1024.0 / 1024.0
            });
        assert!(count > 2);
        assert!(range > LIVE_BYTES_PLATEAU_RANGE_MIB);
        assert!(slope > LIVE_BYTES_PLATEAU_SLOPE_MIB_PER_MINUTE);
        assert!(
            !growth.monotonic_growth(0),
            "the all-observation tracker must retain a dip even if thinning removes it"
        );
    }

    #[test]
    fn soak_growth_analysis_reports_slopes_plateau_and_stale_owners() {
        let mut run = ScenarioRun::new(ScenarioConfig {
            scenario: PerformanceScenario::Soak10Minutes,
            warmup_seconds: 30.0,
            measurement_seconds: 600.0,
            seed: 1,
            run_id: "test".to_string(),
            uncapped_present_mode: false,
        });
        for (elapsed_seconds, entities, resident_gib, live_bytes) in [
            (0.0, 100, 0.500, 100_u64),
            (300.0, 105, 0.510, 300),
            (540.0, 103, 0.505, 250),
            (600.0, 103, 0.505, 250),
        ] {
            run.resource_growth.push(ResourceGrowthSample {
                elapsed_seconds,
                resources: ResourceCounts {
                    entities,
                    meshes: 10,
                    materials: 20,
                    images: 30,
                },
                resident_gib,
                live_bytes,
                stale_owner_entities: 0,
            });
        }
        let analysis = resource_growth_analysis(&run);
        assert_eq!(analysis.sample_count, 4);
        assert!(analysis.live_mib_slope_per_minute > 0.0);
        assert!(!analysis.entity_monotonic_growth);
        assert!(!analysis.live_monotonic_growth);
        assert!(analysis.final_asset_window_plateau);
        assert!(analysis.final_resident_window_plateau);
        assert!(analysis.final_live_window_plateau);
        assert!(!resource_stability_valid(&run));
        run.process_memory_gib.push(0.505);
        assert!(resource_stability_valid(&run));

        run.stale_owner_entities_peak = 1;
        assert!(!resource_stability_valid(&run));
    }

    #[test]
    fn final_soak_window_enforces_explicit_rss_and_live_byte_limits() {
        let mut run = ScenarioRun::new(ScenarioConfig {
            scenario: PerformanceScenario::Soak10Minutes,
            warmup_seconds: 30.0,
            measurement_seconds: 600.0,
            seed: 1,
            run_id: "test".to_string(),
            uncapped_present_mode: false,
        });
        for (elapsed_seconds, resident_gib, live_bytes) in [
            (0.0, 0.600, 3 * 1024 * 1024),
            (540.0, 0.500, 0),
            (600.0, 0.510, 2 * 1024 * 1024),
        ] {
            run.resource_growth.push(ResourceGrowthSample {
                elapsed_seconds,
                resources: ResourceCounts {
                    entities: 100,
                    meshes: 10,
                    materials: 20,
                    images: 30,
                },
                resident_gib,
                live_bytes,
                stale_owner_entities: 0,
            });
        }

        let analysis = resource_growth_analysis(&run);
        assert!(analysis.final_resident_window_range_mib > RSS_PLATEAU_RANGE_MIB);
        assert!(
            analysis.final_resident_window_slope_mib_per_minute.abs()
                > RSS_PLATEAU_SLOPE_MIB_PER_MINUTE
        );
        assert!(!analysis.final_resident_window_plateau);
        assert!(analysis.final_live_window_range_mib > LIVE_BYTES_PLATEAU_RANGE_MIB);
        assert!(
            analysis.final_live_window_slope_mib_per_minute.abs()
                > LIVE_BYTES_PLATEAU_SLOPE_MIB_PER_MINUTE
        );
        assert!(!analysis.final_live_window_plateau);
        run.process_memory_gib.push(0.510);
        assert!(!resource_stability_valid(&run));
    }

    #[test]
    fn map_acceptance_requires_one_latency_sample_per_switch_and_a_stable_cycle() {
        let mut run = ScenarioRun::new(ScenarioConfig {
            scenario: PerformanceScenario::MapCycle100,
            warmup_seconds: 30.0,
            measurement_seconds: 300.0,
            seed: 1,
            run_id: "test".to_string(),
            uncapped_present_mode: false,
        });
        assert!(!run.map_cycle_preload_ready);
        assert!(
            fixture_invalid_reasons(&run).contains(&"map_cycle_preload_not_ready"),
            "MapCycle acceptance must remain closed until both recursive folder loads finish"
        );
        let stable = ResourceCounts {
            entities: 400,
            meshes: 100,
            materials: 80,
            images: 30,
        };
        run.map_switches = MAP_SWITCH_COUNT;
        run.switch_ms = vec![1.0; MAP_SWITCH_COUNT];
        run.map_measured_present_ack_count = MAP_SWITCH_COUNT;
        run.map_first_cycle_resources = Some(stable);
        run.entity_end = stable.entities;
        run.mesh_end = stable.meshes;
        run.material_end = stable.materials;
        run.image_end = stable.images;
        run.map_cycle_checkpoints = (0..MAP_ALIGNED_CHECKPOINT_COUNT)
            .map(|cycle| MapCycleCheckpoint {
                cycle,
                present_epoch: cycle as u64 + 1,
                elapsed_seconds: cycle as f64 * 30.0,
                resources: stable,
                resident_gib: Some(0.5),
                live_bytes: 1024,
            })
            .collect();
        assert!(map_switch_samples_valid(&run));
        assert!(resource_stability_valid(&run));

        run.switch_ms.pop();
        assert!(!map_switch_samples_valid(&run));
        run.switch_ms.push(1.0);
        run.image_end += 1;
        assert!(!resource_stability_valid(&run));
    }

    #[test]
    fn map_growth_uses_the_exact_last_four_of_eleven_aligned_checkpoints() {
        let mut run = ScenarioRun::new(ScenarioConfig {
            scenario: PerformanceScenario::MapCycle100,
            warmup_seconds: DEFAULT_WARMUP_SECONDS,
            measurement_seconds: DEFAULT_STRESS_SECONDS,
            seed: DEFAULT_REPLAY_SEED,
            run_id: "aligned-growth".to_string(),
            uncapped_present_mode: false,
        });
        run.measurement_started = true;
        run.measurement_elapsed = DEFAULT_STRESS_SECONDS;
        let stable = ResourceCounts {
            entities: 400,
            meshes: 100,
            materials: 80,
            images: 30,
        };
        run.map_cycle_checkpoints = (0..MAP_ALIGNED_CHECKPOINT_COUNT)
            .map(|cycle| MapCycleCheckpoint {
                cycle,
                present_epoch: cycle as u64 * 10 + 11,
                elapsed_seconds: cycle as f64 * 30.0,
                resources: stable,
                resident_gib: Some(0.5 + cycle as f64 * 0.5 / 1024.0),
                live_bytes: 64 * 1024 * 1024 + cycle as u64 * 64 * 1024,
            })
            .collect();

        let aligned = aligned_cycle_growth_analysis(&run);
        assert_eq!(aligned.checkpoint_count, MAP_ALIGNED_CHECKPOINT_COUNT);
        assert_eq!(aligned.interval_count, 3);
        assert_eq!(aligned.span_seconds, 90.0);
        assert!(aligned.resource_counts_valid);
        assert!(aligned.resident_plateau);
        assert!(aligned.live_plateau);
        assert_eq!(
            aligned_rss_growth_acceptance_status(&run, aligned),
            "passed"
        );

        // A large dip inside the authoritative four-checkpoint tail remains in
        // both the range and regression. It cannot be substituted away by a
        // convenient pair of endpoints or by a monotonicity exception.
        run.map_cycle_checkpoints[8].resident_gib = Some(0.45);
        let with_dip = aligned_cycle_growth_analysis(&run);
        assert!(with_dip.resident_range_mib > RSS_PLATEAU_RANGE_MIB);
        assert!(!with_dip.resident_plateau);
        assert_eq!(
            aligned_rss_growth_acceptance_status(&run, with_dip),
            "failed"
        );

        run.map_cycle_checkpoints.pop();
        let incomplete = aligned_cycle_growth_analysis(&run);
        assert_eq!(incomplete.checkpoint_count, 10);
        assert_eq!(
            aligned_rss_growth_acceptance_status(&run, incomplete),
            "insufficient_aligned_cycle_evidence"
        );
    }

    #[test]
    fn present_acknowledgement_contract_is_explicit_for_fixed_and_map_fixtures() {
        let mut fixed = ScenarioRun::new(ScenarioConfig {
            scenario: PerformanceScenario::FourBotStress,
            warmup_seconds: 1.0,
            measurement_seconds: 1.0,
            seed: 1,
            run_id: "fixed-acks".to_string(),
            uncapped_present_mode: false,
        });
        fixed.initial_present_ack_count = 1;
        fixed.final_present_ack_count = 1;
        fixed.present_ack_count = 2;
        fixed.final_present_requested_epoch = 2;
        fixed.final_present_observed_epoch = 2;
        assert!(present_acknowledgements_valid(&fixed));

        let mut map = ScenarioRun::new(ScenarioConfig {
            scenario: PerformanceScenario::MapCycle100,
            warmup_seconds: 1.0,
            measurement_seconds: 1.0,
            seed: 1,
            run_id: "map-acks".to_string(),
            uncapped_present_mode: false,
        });
        map.initial_present_ack_count = 1;
        map.map_warm_precycle_present_ack_count = MAP_WARM_PRECYCLE_SWITCH_COUNT;
        map.map_measured_present_ack_count = MAP_SWITCH_COUNT;
        map.final_present_ack_count = 1;
        map.present_ack_count = 1 + MAP_WARM_PRECYCLE_SWITCH_COUNT + MAP_SWITCH_COUNT;
        map.final_present_requested_epoch = 111;
        map.final_present_observed_epoch = 111;
        assert!(present_acknowledgements_valid(&map));
        map.present_ack_count -= 1;
        assert!(!present_acknowledgements_valid(&map));
    }

    #[test]
    fn final_map_switch_remains_driveable_until_its_present_sample_finishes() {
        let mut run = ScenarioRun::new(ScenarioConfig {
            scenario: PerformanceScenario::MapCycle100,
            warmup_seconds: 0.0,
            measurement_seconds: 1.0,
            seed: 1,
            run_id: "final-switch".to_string(),
            uncapped_present_mode: false,
        });
        run.phase = MeasurementPhase::Measure;
        run.map_switches = MAP_SWITCH_COUNT;
        run.pending_switch_started = Some(Instant::now());
        assert!(map_cycle_requires_drive(&run));

        run.pending_switch_started = None;
        run.pending_present_epoch = Some(7);
        assert!(map_cycle_requires_drive(&run));

        run.pending_present_epoch = None;
        assert!(!map_cycle_requires_drive(&run));
    }
}
