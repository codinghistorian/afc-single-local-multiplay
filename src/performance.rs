//! Opt-in runtime diagnostics for repeatable performance measurements.
//!
//! This module intentionally collects engine-level metrics only. Benchmark
//! scenarios remain explicit gameplay procedures so enabling `perf` cannot
//! modify match setup or fighter behavior.

use std::time::Duration;

use bevy::diagnostic::{
    DiagnosticsStore, EntityCountDiagnosticsPlugin, FrameTimeDiagnosticsPlugin,
    LogDiagnosticsPlugin,
};
use bevy::prelude::*;
use bevy::render::diagnostic::{MeshAllocatorDiagnosticPlugin, RenderDiagnosticsPlugin};

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use crate::arena_defs::{arena_definitions, set_active_arena_index};
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use crate::characters::CharacterKind;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use crate::components::{
    BotBehaviorMode, BotBrain, Controller, LocalInputAssignment, ParticipantKind,
};
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use crate::constants::FIGHTER_COUNT;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use crate::game_state::{DEFAULT_REPLAY_SEED, LocalSetup, MatchPhase, MatchState};
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use crate::styles::FighterStyleKind;

/// Default interval between diagnostic snapshots written to the application log.
pub const DEFAULT_LOG_INTERVAL: Duration = Duration::from_secs(2);

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

/// Registers frame-time, FPS, frame-count, and entity-count diagnostics.
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

        app.insert_resource(self.config.clone()).add_plugins((
            FrameTimeDiagnosticsPlugin::new(history_length),
            EntityCountDiagnosticsPlugin::new(history_length),
            RenderDiagnosticsPlugin,
            MeshAllocatorDiagnosticPlugin,
            LogDiagnosticsPlugin {
                wait_duration: self.config.log_interval,
                ..default()
            },
        ));

        #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
        if let Some(run) = FourBotStressRun::from_environment() {
            app.insert_resource(run)
                .add_systems(
                    Startup,
                    configure_four_bot_stress
                        .before(crate::arena::setup_arena)
                        .before(crate::items::setup_items)
                        .before(crate::fighter::spawn_fighters),
                )
                .add_systems(
                    Update,
                    (keep_four_bot_stress_running, collect_four_bot_stress).chain(),
                );
        }
    }
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
#[derive(Resource, Debug)]
struct FourBotStressRun {
    warmup_secs: f64,
    sample_secs: f64,
    fighting_started_at: Option<f64>,
    sampling_announced: bool,
    frame_ms: Vec<f64>,
    render_cpu_ms: Vec<f64>,
    render_gpu_ms: Vec<f64>,
    process_cpu_start_secs: Option<f64>,
    process_cpu_end_secs: Option<f64>,
    process_memory_peak_gib: f64,
    process_memory_end_gib: f64,
    entity_peak: f64,
    entity_end: f64,
    mesh_allocations_peak: f64,
    mesh_allocations_end: f64,
    mesh_count_peak: usize,
    material_count_peak: usize,
    image_count_peak: usize,
    scene_count_peak: usize,
    mesh_count_end: usize,
    material_count_end: usize,
    image_count_end: usize,
    scene_count_end: usize,
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
impl FourBotStressRun {
    const DEFAULT_WARMUP_SECS: f64 = 30.0;
    const DEFAULT_SAMPLE_SECS: f64 = 300.0;

    fn from_environment() -> Option<Self> {
        if std::env::var("AFC_PERF_SCENARIO").ok().as_deref() != Some("FourBotStress") {
            return None;
        }
        let seconds = |name: &str, fallback: f64| {
            std::env::var(name)
                .ok()
                .and_then(|value| value.parse::<f64>().ok())
                .filter(|value| value.is_finite() && *value > 0.0)
                .unwrap_or(fallback)
        };
        Some(Self {
            warmup_secs: seconds("AFC_PERF_WARMUP_SECS", Self::DEFAULT_WARMUP_SECS),
            sample_secs: seconds("AFC_PERF_SAMPLE_SECS", Self::DEFAULT_SAMPLE_SECS),
            fighting_started_at: None,
            sampling_announced: false,
            frame_ms: Vec::new(),
            render_cpu_ms: Vec::new(),
            render_gpu_ms: Vec::new(),
            process_cpu_start_secs: None,
            process_cpu_end_secs: None,
            process_memory_peak_gib: 0.0,
            process_memory_end_gib: 0.0,
            entity_peak: 0.0,
            entity_end: 0.0,
            mesh_allocations_peak: 0.0,
            mesh_allocations_end: 0.0,
            mesh_count_peak: 0,
            material_count_peak: 0,
            image_count_peak: 0,
            scene_count_peak: 0,
            mesh_count_end: 0,
            material_count_end: 0,
            image_count_end: 0,
            scene_count_end: 0,
        })
    }
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn configure_four_bot_stress(
    run: Res<FourBotStressRun>,
    mut setup: ResMut<LocalSetup>,
    mut state: ResMut<MatchState>,
) {
    let arena_index = arena_definitions()
        .iter()
        .enumerate()
        .max_by_key(|(_, arena)| {
            (
                arena.item_anchors.len() + arena.hazards.len(),
                arena.hazards.len(),
                arena.item_anchors.len(),
            )
        })
        .map(|(index, _)| index)
        .unwrap_or(0);
    let stress_characters = [
        CharacterKind::Cat,
        CharacterKind::Pig,
        CharacterKind::Bee,
        CharacterKind::Penguin,
    ];
    setup.set_rule(1);
    setup.arena_index = arena_index;
    setup.replay_seed = DEFAULT_REPLAY_SEED;
    for (slot, character) in setup.slots.iter_mut().zip(stress_characters) {
        slot.participant = ParticipantKind::Bot;
        slot.input = LocalInputAssignment::Unassigned;
        slot.character = character;
        slot.style = FighterStyleKind::Catalyst;
    }
    state.rule_index = setup.rule_index;
    state.rules = setup.active_rule();
    state.arena_index = arena_index;
    state.replay_seed = setup.replay_seed;
    state.apply_local_setup(&setup);
    set_active_arena_index(arena_index);
    state.request_rematch();
    println!(
        "FOUR_BOT_STRESS_CONFIG arena={} items={} hazards={} seed={:#x} warmup_s={:.0} sample_s={:.0}",
        arena_definitions()[arena_index].name,
        arena_definitions()[arena_index].item_anchors.len(),
        arena_definitions()[arena_index].hazards.len(),
        setup.replay_seed,
        run.warmup_secs,
        run.sample_secs,
    );
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn keep_four_bot_stress_running(
    mut state: ResMut<MatchState>,
    mut bots: Query<(&Controller, &mut BotBrain)>,
) {
    for (controller, mut brain) in &mut bots {
        if controller.is_bot() && brain.behavior != BotBehaviorMode::Combatant {
            crate::bot::start_bot_combat_ai(&mut brain);
        }
    }
    if state.phase == MatchPhase::Results {
        state.request_rematch();
    }
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
#[allow(clippy::too_many_arguments)]
fn collect_four_bot_stress(
    time: Res<Time<Real>>,
    state: Res<MatchState>,
    bots: Query<(&Controller, &BotBrain)>,
    diagnostics: Res<DiagnosticsStore>,
    meshes: Res<Assets<Mesh>>,
    materials: Res<Assets<StandardMaterial>>,
    images: Res<Assets<Image>>,
    scenes: Res<Assets<Scene>>,
    mut run: ResMut<FourBotStressRun>,
    mut app_exit: MessageWriter<AppExit>,
) {
    let active_combatants = bots
        .iter()
        .filter(|(controller, brain)| {
            controller.is_bot() && brain.behavior == BotBehaviorMode::Combatant
        })
        .count();
    if state.phase != MatchPhase::Fighting || active_combatants != FIGHTER_COUNT {
        return;
    }

    let now = time.elapsed_secs_f64();
    let started_at = *run.fighting_started_at.get_or_insert(now);
    let elapsed = now - started_at;
    if elapsed < run.warmup_secs {
        return;
    }
    if !run.sampling_announced {
        run.sampling_announced = true;
        println!(
            "FOUR_BOT_STRESS_SAMPLE_BEGIN warmup_s={:.0} sample_s={:.0}",
            run.warmup_secs, run.sample_secs
        );
    }

    let sample_elapsed = elapsed - run.warmup_secs;
    if sample_elapsed >= run.sample_secs {
        print_four_bot_stress_result(&run);
        app_exit.write(AppExit::Success);
        return;
    }

    let frame_ms = time.delta_secs_f64() * 1000.0;
    if frame_ms.is_finite() && frame_ms > 0.0 {
        run.frame_ms.push(frame_ms);
    }
    if let Some(process) = process_sample() {
        run.process_cpu_start_secs
            .get_or_insert(process.cpu_seconds);
        run.process_cpu_end_secs = Some(process.cpu_seconds);
        run.process_memory_peak_gib = run.process_memory_peak_gib.max(process.resident_memory_gib);
        run.process_memory_end_gib = process.resident_memory_gib;
    }
    if let Some(value) =
        latest_diagnostic(&diagnostics, &EntityCountDiagnosticsPlugin::ENTITY_COUNT)
    {
        run.entity_peak = run.entity_peak.max(value);
        run.entity_end = value;
    }
    if let Some(value) = latest_diagnostic(
        &diagnostics,
        MeshAllocatorDiagnosticPlugin::allocations_diagnostic_path(),
    ) {
        run.mesh_allocations_peak = run.mesh_allocations_peak.max(value);
        run.mesh_allocations_end = value;
    }

    if let Some(value) = render_diagnostic_max(&diagnostics, "elapsed_cpu") {
        run.render_cpu_ms.push(value);
    }
    if let Some(value) = render_diagnostic_max(&diagnostics, "elapsed_gpu") {
        run.render_gpu_ms.push(value);
    }

    run.mesh_count_end = meshes.len();
    run.material_count_end = materials.len();
    run.image_count_end = images.len();
    run.scene_count_end = scenes.len();
    run.mesh_count_peak = run.mesh_count_peak.max(run.mesh_count_end);
    run.material_count_peak = run.material_count_peak.max(run.material_count_end);
    run.image_count_peak = run.image_count_peak.max(run.image_count_end);
    run.scene_count_peak = run.scene_count_peak.max(run.scene_count_end);
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn latest_diagnostic(
    diagnostics: &DiagnosticsStore,
    path: &bevy::diagnostic::DiagnosticPath,
) -> Option<f64> {
    diagnostics
        .get(path)
        .and_then(|diagnostic| diagnostic.value())
        .filter(|value| value.is_finite())
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn render_diagnostic_max(diagnostics: &DiagnosticsStore, suffix: &str) -> Option<f64> {
    diagnostics
        .iter()
        .filter(|diagnostic| {
            diagnostic.path().as_str().starts_with("render/")
                && diagnostic.path().as_str().ends_with(suffix)
        })
        .filter_map(|diagnostic| diagnostic.value())
        .filter(|value| value.is_finite())
        .max_by(f64::total_cmp)
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
#[derive(Clone, Copy)]
struct ProcessSample {
    cpu_seconds: f64,
    resident_memory_gib: f64,
}

#[cfg(all(feature = "native", target_os = "macos", not(target_arch = "wasm32")))]
#[allow(deprecated)]
fn process_sample() -> Option<ProcessSample> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: `usage` points to writable storage for the exact structure expected by getrusage.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: getrusage returned success and initialized the output structure.
    let usage = unsafe { usage.assume_init() };
    let cpu_seconds = usage.ru_utime.tv_sec as f64
        + usage.ru_utime.tv_usec as f64 / 1_000_000.0
        + usage.ru_stime.tv_sec as f64
        + usage.ru_stime.tv_usec as f64 / 1_000_000.0;

    let mut info = std::mem::MaybeUninit::<libc::mach_task_basic_info>::zeroed();
    let mut count = libc::MACH_TASK_BASIC_INFO_COUNT;
    // SAFETY: the task port belongs to this process, the flavor matches `info`, and `count`
    // describes the output buffer in natural-word units as required by Mach.
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
    // SAFETY: task_info returned success and initialized the packed structure. The field is read
    // unaligned because Darwin declares this ABI structure with four-byte packing.
    let resident_bytes =
        unsafe { std::ptr::addr_of!((*info.as_ptr()).resident_size).read_unaligned() };
    Some(ProcessSample {
        cpu_seconds,
        resident_memory_gib: resident_bytes as f64 / 1024.0_f64.powi(3),
    })
}

#[cfg(all(
    feature = "native",
    not(target_os = "macos"),
    not(target_arch = "wasm32")
))]
fn process_sample() -> Option<ProcessSample> {
    None
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn percentile(samples: &[f64], percentile: f64) -> f64 {
    if samples.is_empty() {
        return f64::NAN;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let index = ((sorted.len() - 1) as f64 * percentile)
        .round()
        .clamp(0.0, (sorted.len() - 1) as f64) as usize;
    sorted[index]
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn print_four_bot_stress_result(run: &FourBotStressRun) {
    let frame = (
        percentile(&run.frame_ms, 0.5),
        percentile(&run.frame_ms, 0.95),
        percentile(&run.frame_ms, 0.99),
    );
    let render_cpu = (
        percentile(&run.render_cpu_ms, 0.5),
        percentile(&run.render_cpu_ms, 0.95),
        percentile(&run.render_cpu_ms, 0.99),
    );
    let render_gpu = (
        percentile(&run.render_gpu_ms, 0.5),
        percentile(&run.render_gpu_ms, 0.95),
        percentile(&run.render_gpu_ms, 0.99),
    );
    let process_cpu_total_percent = run
        .process_cpu_start_secs
        .zip(run.process_cpu_end_secs)
        .map(|(start, end)| (end - start).max(0.0) / run.sample_secs * 100.0)
        .unwrap_or(f64::NAN);
    let logical_cpus = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(1) as f64;
    println!(
        concat!(
            "FOUR_BOT_STRESS_RESULT ",
            "samples={} frame_median_ms={:.4} frame_p95_ms={:.4} frame_p99_ms={:.4} ",
            "render_cpu_span_median_ms={:.4} render_cpu_span_p95_ms={:.4} ",
            "render_cpu_span_p99_ms={:.4} render_gpu_median_ms={:.4} ",
            "render_gpu_p95_ms={:.4} render_gpu_p99_ms={:.4} ",
            "process_cpu_total_percent={:.2} process_cpu_normalized_percent={:.2} ",
            "process_memory_peak_gib={:.4} process_memory_end_gib={:.4} ",
            "entities_peak={:.0} entities_end={:.0} mesh_allocations_peak={:.0} ",
            "mesh_allocations_end={:.0} meshes_peak={} meshes_end={} ",
            "materials_peak={} materials_end={} images_peak={} images_end={} ",
            "scenes_peak={} scenes_end={}"
        ),
        run.frame_ms.len(),
        frame.0,
        frame.1,
        frame.2,
        render_cpu.0,
        render_cpu.1,
        render_cpu.2,
        render_gpu.0,
        render_gpu.1,
        render_gpu.2,
        process_cpu_total_percent,
        process_cpu_total_percent / logical_cpus,
        run.process_memory_peak_gib,
        run.process_memory_end_gib,
        run.entity_peak,
        run.entity_end,
        run.mesh_allocations_peak,
        run.mesh_allocations_end,
        run.mesh_count_peak,
        run.mesh_count_end,
        run.material_count_peak,
        run.material_count_end,
        run.image_count_peak,
        run.image_count_end,
        run.scene_count_peak,
        run.scene_count_end,
    );
}
