//! Opt-in, render-free multiplayer acceptance profiler.
//!
//! This module is compiled only with the `perf` feature. It measures the real
//! production authority and rollback paths without adding counters or clocks to
//! normal gameplay builds.

use std::fmt;
use std::time::Instant;

use crate::arena_defs::arena_definitions;
use crate::authority::{AUTHORITY_SNAPSHOT_HISTORY_TICKS, AuthorityMatch, AuthoritySimulation};
use crate::authority_input::{
    AuthorityInputConfig, AuthorityInputOrigin, AuthorityInputRecord, AuthorityInputStatus,
    CommittedTickInputs,
};
use crate::dedicated_server::DedicatedLaunchOptions;
use crate::game_state::{DEFAULT_REPLAY_SEED, MatchState};
use crate::headless::{HeadlessMatchConfig, build_headless_simulation, build_predicted_simulation};
use crate::network_protocol::{
    InputFrame, MAX_SEATS, MatchId, MatchManifest, QuantizedAxis, SeatId, SeatOwner, SimTick,
};
use crate::performance::allocation_snapshot;
use crate::rollback::{
    LateInputOutcome, NORMAL_ROLLBACK_LIMIT_TICKS, NoopEventDiscard, NoopRollbackTiming,
    PredictionEngine,
};

pub const AUTHORITY_P99_BUDGET_NS: u64 = 1_000_000;
pub const ROLLBACK_P99_BUDGET_NS: u64 = 4_000_000;
pub const PROFILE_ROLLBACK_DEPTH_TICKS: u64 = 12;

const DEFAULT_AUTHORITY_WARMUP_TICKS: u32 = 256;
const DEFAULT_ROLLBACK_WARMUP_BURSTS: u32 = 16;
const DEFAULT_SAMPLE_COUNT: usize = 1_000;
const MIN_SAMPLE_COUNT: usize = 100;
const MAX_SAMPLE_COUNT: usize = 1_000_000;
const MAX_WARMUP_COUNT: u32 = 1_000_000;
// The canonical match codec stores stocks as u8. Use its largest valid value so
// a profiling fixture survives ordinary combat without leaving the real schema.
const PROFILE_STOCKS: i32 = u8::MAX as i32;
const PROFILE_MATCH_ID: [u8; 16] = *b"mp-perf-prof-v1!";

pub const MULTIPLAYER_PROFILE_HELP: &str = "\
Render-free multiplayer performance acceptance

Usage:
  afc-multiplayer-profile --hardware <description> [options]

Required:
  --hardware <text>                  Stable CPU/machine/OS description

Options:
  --run-id <text>                    Evidence label or commit/run ID (default: unlabeled)
  --seed <u64|0xHEX>                 Deterministic workload seed
  --authority-warmup-ticks <count>   Untimed authority steps (default: 256)
  --rollback-warmup-bursts <count>   Untimed 12-tick corrections (default: 16)
  --samples <count>                  Samples for each p99 distribution (default: 1000; min: 100)
  --allocation-breakdown             Allocation-only production phase diagnosis
  --report-only                      Emit failures but exit successfully
  -h, --help                         Print this help

The process emits exactly one AFC_MULTIPLAYER_PERF_RESULT JSON record after a
successful run. Without --report-only it exits nonzero when an acceptance budget
is missed.
";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MultiplayerProfileConfig {
    pub hardware: String,
    pub run_id: String,
    pub seed: u64,
    pub authority_warmup_ticks: u32,
    pub rollback_warmup_bursts: u32,
    pub samples: usize,
    pub allocation_breakdown: bool,
    pub report_only: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MultiplayerProfileCliAction {
    Run(MultiplayerProfileConfig),
    Help,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MultiplayerProfileError {
    Arguments(String),
    Runtime(String),
}

impl fmt::Display for MultiplayerProfileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Arguments(message) => {
                write!(
                    formatter,
                    "invalid multiplayer profiler arguments: {message}"
                )
            }
            Self::Runtime(message) => {
                write!(formatter, "multiplayer profiler failed: {message}")
            }
        }
    }
}

impl std::error::Error for MultiplayerProfileError {}

pub fn parse_multiplayer_profile_args<I, S>(
    args: I,
) -> Result<MultiplayerProfileCliAction, MultiplayerProfileError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut hardware = None;
    let mut run_id = None;
    let mut seed = None;
    let mut authority_warmup_ticks = None;
    let mut rollback_warmup_bursts = None;
    let mut samples = None;
    let mut allocation_breakdown = false;
    let mut report_only = false;
    let mut arguments = args.into_iter().map(Into::into);

    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "-h" | "--help" => return Ok(MultiplayerProfileCliAction::Help),
            "--report-only" => {
                if report_only {
                    return Err(argument_error("duplicate --report-only"));
                }
                report_only = true;
            }
            "--allocation-breakdown" => {
                if allocation_breakdown {
                    return Err(argument_error("duplicate --allocation-breakdown"));
                }
                allocation_breakdown = true;
            }
            "--hardware" => set_once(
                &mut hardware,
                next_value(&mut arguments, "--hardware")?,
                "--hardware",
            )?,
            "--run-id" => set_once(
                &mut run_id,
                next_value(&mut arguments, "--run-id")?,
                "--run-id",
            )?,
            "--seed" => {
                let value = next_value(&mut arguments, "--seed")?;
                set_once(
                    &mut seed,
                    parse_seed(&value)
                        .ok_or_else(|| argument_error(format!("invalid --seed value {value:?}")))?,
                    "--seed",
                )?;
            }
            "--authority-warmup-ticks" => {
                let value = next_value(&mut arguments, "--authority-warmup-ticks")?;
                set_once(
                    &mut authority_warmup_ticks,
                    parse_bounded_u32(&value, MAX_WARMUP_COUNT).ok_or_else(|| {
                        argument_error(format!(
                            "--authority-warmup-ticks must be in 0..={MAX_WARMUP_COUNT}"
                        ))
                    })?,
                    "--authority-warmup-ticks",
                )?;
            }
            "--rollback-warmup-bursts" => {
                let value = next_value(&mut arguments, "--rollback-warmup-bursts")?;
                set_once(
                    &mut rollback_warmup_bursts,
                    parse_bounded_u32(&value, MAX_WARMUP_COUNT).ok_or_else(|| {
                        argument_error(format!(
                            "--rollback-warmup-bursts must be in 0..={MAX_WARMUP_COUNT}"
                        ))
                    })?,
                    "--rollback-warmup-bursts",
                )?;
            }
            "--samples" => {
                let value = next_value(&mut arguments, "--samples")?;
                let parsed = value
                    .parse::<usize>()
                    .ok()
                    .filter(|count| (MIN_SAMPLE_COUNT..=MAX_SAMPLE_COUNT).contains(count));
                set_once(
                    &mut samples,
                    parsed.ok_or_else(|| {
                        argument_error(format!(
                            "--samples must be in {MIN_SAMPLE_COUNT}..={MAX_SAMPLE_COUNT}"
                        ))
                    })?,
                    "--samples",
                )?;
            }
            _ => {
                return Err(argument_error(format!("unknown argument {argument:?}")));
            }
        }
    }

    let hardware = hardware
        .map(|value: String| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| argument_error("--hardware is required and cannot be empty"))?;
    let run_id = run_id
        .map(|value: String| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unlabeled".to_owned());

    Ok(MultiplayerProfileCliAction::Run(MultiplayerProfileConfig {
        hardware,
        run_id,
        seed: seed.unwrap_or(DEFAULT_REPLAY_SEED),
        authority_warmup_ticks: authority_warmup_ticks.unwrap_or(DEFAULT_AUTHORITY_WARMUP_TICKS),
        rollback_warmup_bursts: rollback_warmup_bursts.unwrap_or(DEFAULT_ROLLBACK_WARMUP_BURSTS),
        samples: samples.unwrap_or(DEFAULT_SAMPLE_COUNT),
        allocation_breakdown,
        report_only,
    }))
}

fn next_value(
    arguments: &mut impl Iterator<Item = String>,
    option: &'static str,
) -> Result<String, MultiplayerProfileError> {
    arguments
        .next()
        .filter(|value| !value.starts_with("--"))
        .ok_or_else(|| argument_error(format!("{option} requires a value")))
}

fn set_once<T>(
    destination: &mut Option<T>,
    value: T,
    option: &'static str,
) -> Result<(), MultiplayerProfileError> {
    if destination.replace(value).is_some() {
        Err(argument_error(format!("duplicate {option}")))
    } else {
        Ok(())
    }
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

fn parse_bounded_u32(value: &str, maximum: u32) -> Option<u32> {
    value.parse::<u32>().ok().filter(|value| *value <= maximum)
}

fn argument_error(message: impl Into<String>) -> MultiplayerProfileError {
    MultiplayerProfileError::Arguments(message.into())
}

fn runtime_error(message: impl Into<String>) -> MultiplayerProfileError {
    MultiplayerProfileError::Runtime(message.into())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TimingDistribution {
    pub samples: usize,
    pub p50_ns: u64,
    pub p95_ns: u64,
    pub p99_ns: u64,
    pub maximum_ns: u64,
}

impl TimingDistribution {
    fn from_samples(samples: &mut [u64]) -> Self {
        if samples.is_empty() {
            return Self::default();
        }
        samples.sort_unstable();
        Self {
            samples: samples.len(),
            p50_ns: percentile_nearest_rank(samples, 50),
            p95_ns: percentile_nearest_rank(samples, 95),
            p99_ns: percentile_nearest_rank(samples, 99),
            maximum_ns: *samples.last().expect("non-empty timing sample set"),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AllocationMeasurement {
    pub allocation_count: u64,
    pub allocated_bytes: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AuthorityProfileResult {
    pub timing: TimingDistribution,
    pub allocations: AllocationMeasurement,
    pub snapshot_history_high_water: usize,
    pub snapshot_history_capacity: usize,
    pub timing_pass: bool,
    pub steady_state_allocation_pass: bool,
    pub history_pass: bool,
    pub pass: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RollbackProfileResult {
    pub timing: TimingDistribution,
    pub allocations: AllocationMeasurement,
    pub requested_depth_ticks: u64,
    pub maximum_depth_ticks: u64,
    pub configured_depth_cap_ticks: u64,
    pub snapshot_history_high_water: usize,
    pub input_history_high_water: usize,
    pub history_capacity: usize,
    pub timing_pass: bool,
    pub allocation_free: bool,
    pub depth_pass: bool,
    pub history_pass: bool,
    pub pass: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MultiplayerProfileResult {
    pub hardware: String,
    pub run_id: String,
    pub seed: u64,
    pub authority_warmup_ticks: u32,
    pub rollback_warmup_bursts: u32,
    pub authority: AuthorityProfileResult,
    pub rollback: RollbackProfileResult,
    pub acceptance_pass: bool,
}

impl MultiplayerProfileResult {
    pub fn machine_record(&self) -> String {
        let hardware = json_string(&self.hardware);
        let run_id = json_string(&self.run_id);
        let build_profile = if cfg!(debug_assertions) {
            "debug"
        } else {
            "optimized"
        };
        format!(
            concat!(
                "AFC_MULTIPLAYER_PERF_RESULT {{",
                "\"schema\":\"afc-multiplayer-perf-v1\",",
                "\"run_id\":{run_id},\"hardware\":{hardware},",
                "\"os\":\"{os}\",\"arch\":\"{arch}\",",
                "\"build_profile\":\"{build_profile}\",",
                "\"package_version\":\"{package_version}\",",
                "\"seed\":\"0x{seed:016x}\",",
                "\"authority_warmup_ticks\":{authority_warmup_ticks},",
                "\"rollback_warmup_bursts\":{rollback_warmup_bursts},",
                "\"budgets\":{{\"authority_p99_ns\":{authority_budget},",
                "\"rollback_p99_ns\":{rollback_budget},",
                "\"steady_state_allocations\":0,",
                "\"rollback_depth_ticks\":{rollback_depth}}},",
                "\"authority\":{{\"samples\":{authority_samples},",
                "\"p50_ns\":{authority_p50},\"p95_ns\":{authority_p95},",
                "\"p99_ns\":{authority_p99},\"max_ns\":{authority_max},",
                "\"allocation_count\":{authority_allocations},",
                "\"allocated_bytes\":{authority_allocated_bytes},",
                "\"snapshot_history_high_water\":{authority_history_high_water},",
                "\"snapshot_history_capacity\":{authority_history_capacity},",
                "\"timing_pass\":{authority_timing_pass},",
                "\"steady_state_allocation_pass\":{authority_allocation_pass},",
                "\"history_pass\":{authority_history_pass},",
                "\"pass\":{authority_pass}}},",
                "\"rollback\":{{\"samples\":{rollback_samples},",
                "\"p50_ns\":{rollback_p50},\"p95_ns\":{rollback_p95},",
                "\"p99_ns\":{rollback_p99},\"max_ns\":{rollback_max},",
                "\"allocation_count\":{rollback_allocations},",
                "\"allocated_bytes\":{rollback_allocated_bytes},",
                "\"allocation_free\":{rollback_allocation_free},",
                "\"requested_depth_ticks\":{requested_depth},",
                "\"maximum_depth_ticks\":{maximum_depth},",
                "\"configured_depth_cap_ticks\":{configured_depth_cap},",
                "\"snapshot_history_high_water\":{snapshot_history_high_water},",
                "\"input_history_high_water\":{input_history_high_water},",
                "\"history_capacity\":{history_capacity},",
                "\"timing_pass\":{rollback_timing_pass},",
                "\"depth_pass\":{rollback_depth_pass},",
                "\"history_pass\":{rollback_history_pass},",
                "\"pass\":{rollback_pass}}},",
                "\"acceptance_pass\":{acceptance_pass}}}"
            ),
            run_id = run_id,
            hardware = hardware,
            os = std::env::consts::OS,
            arch = std::env::consts::ARCH,
            build_profile = build_profile,
            package_version = env!("CARGO_PKG_VERSION"),
            seed = self.seed,
            authority_warmup_ticks = self.authority_warmup_ticks,
            rollback_warmup_bursts = self.rollback_warmup_bursts,
            authority_budget = AUTHORITY_P99_BUDGET_NS,
            rollback_budget = ROLLBACK_P99_BUDGET_NS,
            rollback_depth = PROFILE_ROLLBACK_DEPTH_TICKS,
            authority_samples = self.authority.timing.samples,
            authority_p50 = self.authority.timing.p50_ns,
            authority_p95 = self.authority.timing.p95_ns,
            authority_p99 = self.authority.timing.p99_ns,
            authority_max = self.authority.timing.maximum_ns,
            authority_allocations = self.authority.allocations.allocation_count,
            authority_allocated_bytes = self.authority.allocations.allocated_bytes,
            authority_history_high_water = self.authority.snapshot_history_high_water,
            authority_history_capacity = self.authority.snapshot_history_capacity,
            authority_timing_pass = self.authority.timing_pass,
            authority_allocation_pass = self.authority.steady_state_allocation_pass,
            authority_history_pass = self.authority.history_pass,
            authority_pass = self.authority.pass,
            rollback_samples = self.rollback.timing.samples,
            rollback_p50 = self.rollback.timing.p50_ns,
            rollback_p95 = self.rollback.timing.p95_ns,
            rollback_p99 = self.rollback.timing.p99_ns,
            rollback_max = self.rollback.timing.maximum_ns,
            rollback_allocations = self.rollback.allocations.allocation_count,
            rollback_allocated_bytes = self.rollback.allocations.allocated_bytes,
            rollback_allocation_free = self.rollback.allocation_free,
            requested_depth = self.rollback.requested_depth_ticks,
            maximum_depth = self.rollback.maximum_depth_ticks,
            configured_depth_cap = self.rollback.configured_depth_cap_ticks,
            snapshot_history_high_water = self.rollback.snapshot_history_high_water,
            input_history_high_water = self.rollback.input_history_high_water,
            history_capacity = self.rollback.history_capacity,
            rollback_timing_pass = self.rollback.timing_pass,
            rollback_depth_pass = self.rollback.depth_pass,
            rollback_history_pass = self.rollback.history_pass,
            rollback_pass = self.rollback.pass,
            acceptance_pass = self.acceptance_pass,
        )
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AllocationPhaseMeasurement {
    pub operations: usize,
    pub allocation_count: u64,
    pub allocated_bytes: u64,
}

impl AllocationPhaseMeasurement {
    fn record(&mut self, before: crate::performance::AllocationSnapshot) {
        let delta = allocation_snapshot().delta_since(before);
        self.operations = self.operations.saturating_add(1);
        self.allocation_count = self.allocation_count.saturating_add(delta.allocation_count);
        self.allocated_bytes = self.allocated_bytes.saturating_add(delta.allocated_bytes);
    }

    fn saturating_sub(self, other: Self) -> Self {
        Self {
            operations: self.operations,
            allocation_count: self.allocation_count.saturating_sub(other.allocation_count),
            allocated_bytes: self.allocated_bytes.saturating_sub(other.allocated_bytes),
        }
    }

    fn saturating_add(self, other: Self) -> Self {
        Self {
            operations: self.operations.max(other.operations),
            allocation_count: self.allocation_count.saturating_add(other.allocation_count),
            allocated_bytes: self.allocated_bytes.saturating_add(other.allocated_bytes),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MultiplayerAllocationDiagnosis {
    pub hardware: String,
    pub run_id: String,
    pub seed: u64,
    pub full_authority: AllocationPhaseMeasurement,
    pub bot_generation: AllocationPhaseMeasurement,
    pub canonical_fixed_step: AllocationPhaseMeasurement,
    pub snapshot_capture: AllocationPhaseMeasurement,
    pub snapshot_hash: AllocationPhaseMeasurement,
    pub authority_unattributed: AllocationPhaseMeasurement,
    pub full_rollback: AllocationPhaseMeasurement,
    pub snapshot_clone: AllocationPhaseMeasurement,
    pub snapshot_restore: AllocationPhaseMeasurement,
}

impl MultiplayerAllocationDiagnosis {
    pub fn machine_record(&self) -> String {
        let hardware = json_string(&self.hardware);
        let run_id = json_string(&self.run_id);
        format!(
            concat!(
                "AFC_MULTIPLAYER_ALLOC_RESULT {{",
                "\"schema\":\"afc-multiplayer-allocation-v1\",",
                "\"run_id\":{run_id},\"hardware\":{hardware},",
                "\"os\":\"{os}\",\"arch\":\"{arch}\",",
                "\"build_profile\":\"{build_profile}\",",
                "\"seed\":\"0x{seed:016x}\",",
                "\"full_authority\":{full_authority},",
                "\"bot_generation\":{bot_generation},",
                "\"canonical_fixed_step\":{canonical_fixed_step},",
                "\"snapshot_capture\":{snapshot_capture},",
                "\"snapshot_hash\":{snapshot_hash},",
                "\"authority_unattributed\":{authority_unattributed},",
                "\"full_rollback\":{full_rollback},",
                "\"snapshot_clone\":{snapshot_clone},",
                "\"snapshot_restore\":{snapshot_restore},",
                "\"fixed_step_allocation_pass\":{fixed_step_allocation_pass}}}"
            ),
            run_id = run_id,
            hardware = hardware,
            os = std::env::consts::OS,
            arch = std::env::consts::ARCH,
            build_profile = if cfg!(debug_assertions) {
                "debug"
            } else {
                "optimized"
            },
            seed = self.seed,
            full_authority = phase_json(self.full_authority),
            bot_generation = phase_json(self.bot_generation),
            canonical_fixed_step = phase_json(self.canonical_fixed_step),
            snapshot_capture = phase_json(self.snapshot_capture),
            snapshot_hash = phase_json(self.snapshot_hash),
            authority_unattributed = phase_json(self.authority_unattributed),
            full_rollback = phase_json(self.full_rollback),
            snapshot_clone = phase_json(self.snapshot_clone),
            snapshot_restore = phase_json(self.snapshot_restore),
            fixed_step_allocation_pass = self.canonical_fixed_step.allocation_count == 0,
        )
    }
}

fn phase_json(measurement: AllocationPhaseMeasurement) -> String {
    format!(
        concat!(
            "{{\"operations\":{},\"allocation_count\":{},",
            "\"allocated_bytes\":{}}}"
        ),
        measurement.operations, measurement.allocation_count, measurement.allocated_bytes,
    )
}

pub fn run_multiplayer_allocation_diagnosis(
    config: &MultiplayerProfileConfig,
) -> Result<MultiplayerAllocationDiagnosis, MultiplayerProfileError> {
    let fixture = profile_match_config(config.seed)?;
    let full_authority = diagnose_full_authority_allocations(&fixture, config)?;
    let (bot_generation, canonical_fixed_step, snapshot_capture, snapshot_hash) =
        diagnose_authority_phase_allocations(&fixture, config)?;
    let attributed = bot_generation
        .saturating_add(canonical_fixed_step)
        .saturating_add(snapshot_capture)
        .saturating_add(snapshot_hash);
    let authority_unattributed = full_authority.saturating_sub(attributed);
    let full_rollback = diagnose_full_rollback_allocations(&fixture, config)?;
    let (snapshot_clone, snapshot_restore) =
        diagnose_snapshot_clone_restore_allocations(&fixture, config)?;

    Ok(MultiplayerAllocationDiagnosis {
        hardware: config.hardware.clone(),
        run_id: config.run_id.clone(),
        seed: config.seed,
        full_authority,
        bot_generation,
        canonical_fixed_step,
        snapshot_capture,
        snapshot_hash,
        authority_unattributed,
        full_rollback,
        snapshot_clone,
        snapshot_restore,
    })
}

pub fn run_multiplayer_profile(
    config: &MultiplayerProfileConfig,
) -> Result<MultiplayerProfileResult, MultiplayerProfileError> {
    let fixture = profile_match_config(config.seed)?;
    let authority = profile_authority(&fixture, config)?;
    let rollback = profile_rollback(&fixture, config)?;
    let acceptance_pass = authority.pass && rollback.pass;
    Ok(MultiplayerProfileResult {
        hardware: config.hardware.clone(),
        run_id: config.run_id.clone(),
        seed: config.seed,
        authority_warmup_ticks: config.authority_warmup_ticks,
        rollback_warmup_bursts: config.rollback_warmup_bursts,
        authority,
        rollback,
        acceptance_pass,
    })
}

fn diagnose_full_authority_allocations(
    fixture: &HeadlessMatchConfig,
    config: &MultiplayerProfileConfig,
) -> Result<AllocationPhaseMeasurement, MultiplayerProfileError> {
    let mut simulation = build_headless_simulation(fixture.clone())
        .map_err(|error| runtime_error(format!("authority diagnosis bootstrap: {error}")))?;
    extend_profile_stocks(&mut simulation);
    let mut authority = AuthorityMatch::new(
        fixture.manifest,
        simulation,
        AuthorityInputConfig::default(),
    )
    .map_err(|error| runtime_error(format!("authority diagnosis construction: {error:?}")))?;
    for _ in 0..config.authority_warmup_ticks {
        let report = authority
            .step()
            .map_err(|error| runtime_error(format!("authority diagnosis warmup: {error:?}")))?;
        ensure_match_continues(report.final_result_id, "authority diagnosis warmup")?;
    }

    let mut measurement = AllocationPhaseMeasurement::default();
    for _ in 0..config.samples {
        let before = allocation_snapshot();
        let report = authority
            .step()
            .map_err(|error| runtime_error(format!("authority diagnosis sample: {error:?}")))?;
        measurement.record(before);
        ensure_match_continues(report.final_result_id, "authority diagnosis sample")?;
    }
    Ok(measurement)
}

fn diagnose_authority_phase_allocations(
    fixture: &HeadlessMatchConfig,
    config: &MultiplayerProfileConfig,
) -> Result<
    (
        AllocationPhaseMeasurement,
        AllocationPhaseMeasurement,
        AllocationPhaseMeasurement,
        AllocationPhaseMeasurement,
    ),
    MultiplayerProfileError,
> {
    let mut world = build_headless_simulation(fixture.clone())
        .map_err(|error| runtime_error(format!("authority phase bootstrap: {error}")))?;
    extend_profile_stocks(&mut world);
    for _ in 0..config.authority_warmup_ticks {
        advance_decomposed_authority_tick(&mut world, &fixture.manifest)?;
    }

    let mut bot_generation = AllocationPhaseMeasurement::default();
    let mut canonical_fixed_step = AllocationPhaseMeasurement::default();
    let mut snapshot_capture = AllocationPhaseMeasurement::default();
    let mut snapshot_hash = AllocationPhaseMeasurement::default();
    for _ in 0..config.samples {
        let tick = world.current_sim_tick().next();

        let before = allocation_snapshot();
        let frames = world
            .generate_authority_bot_frames(tick)
            .map_err(|error| runtime_error(format!("bot generation diagnosis: {error:?}")))?
            .ok_or_else(|| runtime_error("allocation fixture did not produce bot frames"))?;
        bot_generation.record(before);

        let committed = committed_bot_inputs(&fixture.manifest, tick, frames)?;
        let before = allocation_snapshot();
        world
            .step_committed(&committed)
            .map_err(|error| runtime_error(format!("fixed-step diagnosis: {error:?}")))?;
        canonical_fixed_step.record(before);

        let before = allocation_snapshot();
        let snapshot = world
            .capture_live_snapshot()
            .map_err(|error| runtime_error(format!("snapshot capture diagnosis: {error:?}")))?;
        snapshot_capture.record(before);

        let before = allocation_snapshot();
        let hash = snapshot
            .canonical_hash()
            .map_err(|error| runtime_error(format!("snapshot hash diagnosis: {error:?}")))?;
        snapshot_hash.record(before);
        std::hint::black_box(hash);
        ensure_driver_match_continues(&world, "authority phase diagnosis")?;
    }
    Ok((
        bot_generation,
        canonical_fixed_step,
        snapshot_capture,
        snapshot_hash,
    ))
}

fn diagnose_full_rollback_allocations(
    fixture: &HeadlessMatchConfig,
    config: &MultiplayerProfileConfig,
) -> Result<AllocationPhaseMeasurement, MultiplayerProfileError> {
    let history_capacity = usize::from(fixture.manifest.snapshot_history_ticks);
    let mut world = build_predicted_simulation(fixture.clone())
        .map_err(|error| runtime_error(format!("rollback diagnosis bootstrap: {error}")))?;
    extend_profile_stocks(&mut world);
    let mut prediction = PredictionEngine::new(&world, history_capacity)
        .map_err(|error| runtime_error(format!("rollback diagnosis construction: {error:?}")))?;
    let mut discard = NoopEventDiscard;
    let mut timing = NoopRollbackTiming;
    for _ in 0..config.rollback_warmup_bursts {
        prepare_rollback_depth(&mut world, &mut prediction)?;
        apply_depth_correction(&mut world, &mut prediction, &mut discard, &mut timing)?;
    }

    let mut measurement = AllocationPhaseMeasurement::default();
    for _ in 0..config.samples {
        prepare_rollback_depth(&mut world, &mut prediction)?;
        let late = corrected_oldest_frame(&prediction)?;
        let before = allocation_snapshot();
        let outcome = prediction
            .apply_late_input(&mut world, late, &mut discard, &mut timing)
            .map_err(|error| runtime_error(format!("rollback allocation diagnosis: {error:?}")))?;
        measurement.record(before);
        validate_depth_correction(outcome, prediction.predicted_tick())?;
    }
    Ok(measurement)
}

fn diagnose_snapshot_clone_restore_allocations(
    fixture: &HeadlessMatchConfig,
    config: &MultiplayerProfileConfig,
) -> Result<(AllocationPhaseMeasurement, AllocationPhaseMeasurement), MultiplayerProfileError> {
    let mut world = build_predicted_simulation(fixture.clone())
        .map_err(|error| runtime_error(format!("restore diagnosis bootstrap: {error}")))?;
    extend_profile_stocks(&mut world);
    for _ in 0..config.authority_warmup_ticks {
        advance_decomposed_authority_tick(&mut world, &fixture.manifest)?;
    }
    let snapshot = world
        .capture_live_snapshot()
        .map_err(|error| runtime_error(format!("restore diagnosis snapshot: {error:?}")))?;
    // Initialize Bevy restore metadata before the allocation-only sample set.
    world
        .restore_live_snapshot(&snapshot)
        .map_err(|error| runtime_error(format!("restore diagnosis warmup: {error:?}")))?;

    let mut snapshot_clone = AllocationPhaseMeasurement::default();
    for _ in 0..config.samples {
        let before = allocation_snapshot();
        let cloned = snapshot.clone();
        snapshot_clone.record(before);
        std::hint::black_box(cloned.header.tick);
    }

    let mut snapshot_restore = AllocationPhaseMeasurement::default();
    for _ in 0..config.samples {
        let before = allocation_snapshot();
        let report = world
            .restore_live_snapshot(&snapshot)
            .map_err(|error| runtime_error(format!("snapshot restore diagnosis: {error:?}")))?;
        snapshot_restore.record(before);
        std::hint::black_box(report);
    }
    Ok((snapshot_clone, snapshot_restore))
}

fn advance_decomposed_authority_tick(
    world: &mut crate::live_authority::LiveSimulationDriver,
    manifest: &MatchManifest,
) -> Result<(), MultiplayerProfileError> {
    let tick = world.current_sim_tick().next();
    let frames = world
        .generate_authority_bot_frames(tick)
        .map_err(|error| runtime_error(format!("allocation warmup bot generation: {error:?}")))?
        .ok_or_else(|| runtime_error("allocation warmup did not produce bot frames"))?;
    let committed = committed_bot_inputs(manifest, tick, frames)?;
    world
        .step_committed(&committed)
        .map_err(|error| runtime_error(format!("allocation warmup fixed step: {error:?}")))?;
    let snapshot = world
        .capture_live_snapshot()
        .map_err(|error| runtime_error(format!("allocation warmup snapshot: {error:?}")))?;
    std::hint::black_box(
        snapshot
            .canonical_hash()
            .map_err(|error| runtime_error(format!("allocation warmup hash: {error:?}")))?,
    );
    ensure_driver_match_continues(world, "allocation diagnosis warmup")
}

fn committed_bot_inputs(
    manifest: &MatchManifest,
    tick: SimTick,
    frames: [Option<InputFrame>; MAX_SEATS],
) -> Result<CommittedTickInputs, MultiplayerProfileError> {
    let mut committed = CommittedTickInputs {
        tick,
        by_seat: [None; MAX_SEATS],
    };
    for assignment in manifest.ownership.as_slice() {
        if assignment.owner != SeatOwner::AuthorityBot {
            return Err(runtime_error(
                "allocation diagnosis requires authority-bot ownership",
            ));
        }
        let seat_index = usize::from(assignment.seat.get());
        let frame = frames[seat_index].ok_or_else(|| {
            runtime_error(format!(
                "allocation diagnosis is missing bot frame for seat {}",
                assignment.seat.get()
            ))
        })?;
        if frame.tick != tick || frame.seat != assignment.seat {
            return Err(runtime_error("allocation diagnosis bot frame mismatch"));
        }
        committed.by_seat[seat_index] = Some(AuthorityInputRecord {
            frame,
            fighter: assignment.fighter,
            origin: AuthorityInputOrigin::AuthorityBot,
            status: AuthorityInputStatus::Committed,
        });
    }
    Ok(committed)
}

fn ensure_driver_match_continues(
    world: &crate::live_authority::LiveSimulationDriver,
    phase: &'static str,
) -> Result<(), MultiplayerProfileError> {
    let final_result = world
        .final_result_id()
        .map_err(|error| runtime_error(format!("{phase}: {error:?}")))?;
    ensure_match_continues(final_result, phase)
}

fn profile_match_config(seed: u64) -> Result<HeadlessMatchConfig, MultiplayerProfileError> {
    let match_id = MatchId::new(PROFILE_MATCH_ID)
        .map_err(|error| runtime_error(format!("invalid profile match ID: {error:?}")))?;
    let options = DedicatedLaunchOptions {
        match_id,
        master_seed: seed,
        arena_index: stress_arena_index(),
        rule_index: 1,
        bot_fighters: 4,
        smoke_ticks: None,
    };
    options
        .headless_config()
        .map_err(|error| runtime_error(format!("profile match configuration: {error}")))
}

fn stress_arena_index() -> usize {
    arena_definitions()
        .iter()
        .enumerate()
        .max_by_key(|(_, arena)| {
            (
                arena.item_anchors.len() + arena.hazards.len(),
                arena.hazards.len(),
                arena.platforms.len(),
            )
        })
        .map(|(index, _)| index)
        .unwrap_or(0)
}

fn extend_profile_stocks(world: &mut crate::live_authority::LiveSimulationDriver) {
    let mut state = world.world_mut().resource_mut::<MatchState>();
    for index in 0..state.stocks.len() {
        if state.active_slots[index] {
            state.stocks[index] = PROFILE_STOCKS;
        }
    }
}

fn profile_authority(
    fixture: &HeadlessMatchConfig,
    config: &MultiplayerProfileConfig,
) -> Result<AuthorityProfileResult, MultiplayerProfileError> {
    let mut simulation = build_headless_simulation(fixture.clone())
        .map_err(|error| runtime_error(format!("authority world bootstrap: {error}")))?;
    extend_profile_stocks(&mut simulation);
    let mut authority = AuthorityMatch::new(
        fixture.manifest,
        simulation,
        AuthorityInputConfig::default(),
    )
    .map_err(|error| runtime_error(format!("authority construction: {error:?}")))?;

    for _ in 0..config.authority_warmup_ticks {
        let report = authority
            .step()
            .map_err(|error| runtime_error(format!("authority warmup: {error:?}")))?;
        ensure_match_continues(report.final_result_id, "authority warmup")?;
    }

    let mut timings = Vec::with_capacity(config.samples);
    let mut allocations = AllocationMeasurement::default();
    for _ in 0..config.samples {
        let before = allocation_snapshot();
        let started = Instant::now();
        let report = authority.step();
        let elapsed = nanos_u64(started.elapsed().as_nanos());
        let after = allocation_snapshot();
        let delta = after.delta_since(before);

        let report =
            report.map_err(|error| runtime_error(format!("authority sample: {error:?}")))?;
        ensure_match_continues(report.final_result_id, "authority sample")?;
        allocations.allocation_count = allocations
            .allocation_count
            .saturating_add(delta.allocation_count);
        allocations.allocated_bytes = allocations
            .allocated_bytes
            .saturating_add(delta.allocated_bytes);
        timings.push(elapsed);
    }

    let timing = TimingDistribution::from_samples(&mut timings);
    let metrics = authority.metrics();
    let snapshot_history_high_water = usize::from(metrics.snapshot_history_high_water);
    let timing_pass = timing.p99_ns < AUTHORITY_P99_BUDGET_NS;
    let steady_state_allocation_pass = allocations.allocation_count == 0;
    let history_pass = snapshot_history_high_water <= AUTHORITY_SNAPSHOT_HISTORY_TICKS;
    Ok(AuthorityProfileResult {
        timing,
        allocations,
        snapshot_history_high_water,
        snapshot_history_capacity: AUTHORITY_SNAPSHOT_HISTORY_TICKS,
        timing_pass,
        steady_state_allocation_pass,
        history_pass,
        pass: timing_pass && steady_state_allocation_pass && history_pass,
    })
}

fn profile_rollback(
    fixture: &HeadlessMatchConfig,
    config: &MultiplayerProfileConfig,
) -> Result<RollbackProfileResult, MultiplayerProfileError> {
    let configured_depth_cap = u64::from(fixture.manifest.rollback_limit_ticks);
    if configured_depth_cap != PROFILE_ROLLBACK_DEPTH_TICKS
        || configured_depth_cap != NORMAL_ROLLBACK_LIMIT_TICKS
    {
        return Err(runtime_error(format!(
            "profile requires an exact {PROFILE_ROLLBACK_DEPTH_TICKS}-tick normal rollback cap; \
             manifest={configured_depth_cap}, engine={NORMAL_ROLLBACK_LIMIT_TICKS}"
        )));
    }
    let history_capacity = usize::from(fixture.manifest.snapshot_history_ticks);
    let mut world = build_predicted_simulation(fixture.clone())
        .map_err(|error| runtime_error(format!("predicted world bootstrap: {error}")))?;
    extend_profile_stocks(&mut world);
    let mut prediction = PredictionEngine::new(&world, history_capacity)
        .map_err(|error| runtime_error(format!("prediction construction: {error:?}")))?;
    let mut discard = NoopEventDiscard;
    // The outer `Instant` below measures the complete public correction call.
    // Disable the narrower diagnostics hook so a second clock read does not
    // perturb the burst being accepted; no synthetic duration enters the result.
    let mut timing_hook = NoopRollbackTiming;

    for _ in 0..config.rollback_warmup_bursts {
        prepare_rollback_depth(&mut world, &mut prediction)?;
        apply_depth_correction(&mut world, &mut prediction, &mut discard, &mut timing_hook)?;
    }

    let mut timings = Vec::with_capacity(config.samples);
    let mut allocations = AllocationMeasurement::default();
    for _ in 0..config.samples {
        // Generating and predicting the twelve future ticks establishes the
        // correction fixture and is deliberately outside the rollback burst
        // timing/allocation window.
        prepare_rollback_depth(&mut world, &mut prediction)?;
        let late = corrected_oldest_frame(&prediction)?;

        let before = allocation_snapshot();
        let started = Instant::now();
        let outcome = prediction.apply_late_input(&mut world, late, &mut discard, &mut timing_hook);
        let elapsed = nanos_u64(started.elapsed().as_nanos());
        let after = allocation_snapshot();
        let delta = after.delta_since(before);

        let outcome =
            outcome.map_err(|error| runtime_error(format!("rollback sample: {error:?}")))?;
        validate_depth_correction(outcome, prediction.predicted_tick())?;
        allocations.allocation_count = allocations
            .allocation_count
            .saturating_add(delta.allocation_count);
        allocations.allocated_bytes = allocations
            .allocated_bytes
            .saturating_add(delta.allocated_bytes);
        timings.push(elapsed);
    }

    let timing = TimingDistribution::from_samples(&mut timings);
    let metrics = prediction.metrics();
    let timing_pass = timing.p99_ns < ROLLBACK_P99_BUDGET_NS;
    let allocation_free = allocations.allocation_count == 0;
    let depth_pass = metrics.maximum_normal_rollback_depth == PROFILE_ROLLBACK_DEPTH_TICKS
        && metrics.maximum_normal_rollback_depth <= configured_depth_cap;
    let history_pass = prediction.history_capacity() == history_capacity
        && metrics.snapshot_history_high_water <= history_capacity
        && metrics.input_history_high_water <= history_capacity;
    Ok(RollbackProfileResult {
        timing,
        allocations,
        requested_depth_ticks: PROFILE_ROLLBACK_DEPTH_TICKS,
        maximum_depth_ticks: metrics.maximum_normal_rollback_depth,
        configured_depth_cap_ticks: configured_depth_cap,
        snapshot_history_high_water: metrics.snapshot_history_high_water,
        input_history_high_water: metrics.input_history_high_water,
        history_capacity,
        timing_pass,
        allocation_free,
        depth_pass,
        history_pass,
        // The architecture's allocation gate is the normal steady-state step.
        // Rollback allocation is still emitted explicitly for optimization.
        pass: timing_pass && depth_pass && history_pass,
    })
}

fn prepare_rollback_depth(
    world: &mut crate::live_authority::LiveSimulationDriver,
    prediction: &mut PredictionEngine<crate::snapshot::CanonicalSnapshot>,
) -> Result<(), MultiplayerProfileError> {
    for _ in 0..PROFILE_ROLLBACK_DEPTH_TICKS {
        let tick = prediction.predicted_tick().next();
        let frames = world
            .generate_authority_bot_frames(tick)
            .map_err(|error| runtime_error(format!("rollback bot input generation: {error:?}")))?
            .ok_or_else(|| runtime_error("profile fixture did not produce bot input frames"))?;
        prediction
            .predict_next(world, frames)
            .map_err(|error| runtime_error(format!("rollback prediction setup: {error:?}")))?;
    }
    Ok(())
}

fn corrected_oldest_frame(
    prediction: &PredictionEngine<crate::snapshot::CanonicalSnapshot>,
) -> Result<InputFrame, MultiplayerProfileError> {
    let predicted_tick = prediction.predicted_tick();
    let input_tick = SimTick(
        predicted_tick
            .0
            .checked_sub(PROFILE_ROLLBACK_DEPTH_TICKS - 1)
            .ok_or_else(|| runtime_error("rollback fixture timeline underflow"))?,
    );
    let seat = SeatId::new(0)
        .map_err(|error| runtime_error(format!("invalid profile seat: {error:?}")))?;
    let mut frame = *prediction
        .inputs_at(input_tick)
        .ok_or_else(|| runtime_error("rollback fixture input history is missing"))?
        .frame(seat);
    let corrected_axis = if frame.movement_x.get() >= 0 {
        -127
    } else {
        127
    };
    frame.movement_x = QuantizedAxis::new(corrected_axis)
        .map_err(|error| runtime_error(format!("invalid corrected axis: {error:?}")))?;
    Ok(frame)
}

fn apply_depth_correction(
    world: &mut crate::live_authority::LiveSimulationDriver,
    prediction: &mut PredictionEngine<crate::snapshot::CanonicalSnapshot>,
    discard: &mut NoopEventDiscard,
    timing: &mut NoopRollbackTiming,
) -> Result<(), MultiplayerProfileError> {
    let late = corrected_oldest_frame(prediction)?;
    let outcome = prediction
        .apply_late_input(world, late, discard, timing)
        .map_err(|error| runtime_error(format!("rollback warmup: {error:?}")))?;
    validate_depth_correction(outcome, prediction.predicted_tick())
}

fn validate_depth_correction(
    outcome: LateInputOutcome,
    predicted_tick: SimTick,
) -> Result<(), MultiplayerProfileError> {
    match outcome {
        LateInputOutcome::Corrected {
            restored_tick,
            resimulated_through,
            depth_ticks,
            ..
        } if depth_ticks == PROFILE_ROLLBACK_DEPTH_TICKS
            && resimulated_through == predicted_tick
            && predicted_tick.0.saturating_sub(restored_tick.0) == PROFILE_ROLLBACK_DEPTH_TICKS =>
        {
            Ok(())
        }
        other => Err(runtime_error(format!(
            "expected one exact {PROFILE_ROLLBACK_DEPTH_TICKS}-tick correction, found {other:?}"
        ))),
    }
}

fn ensure_match_continues(
    final_result_id: Option<u64>,
    phase: &'static str,
) -> Result<(), MultiplayerProfileError> {
    if final_result_id.is_some() {
        Err(runtime_error(format!(
            "{phase} exhausted the extended-stock profile fixture"
        )))
    } else {
        Ok(())
    }
}

fn nanos_u64(nanoseconds: u128) -> u64 {
    u64::try_from(nanoseconds).unwrap_or(u64::MAX)
}

fn percentile_nearest_rank(sorted: &[u64], percentile: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = percentile.saturating_mul(sorted.len()).div_ceil(100).max(1);
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn json_string(value: &str) -> String {
    let mut output = String::with_capacity(value.len() + 2);
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character if character.is_control() => {
                use std::fmt::Write as _;
                let _ = write!(output, "\\u{:04x}", character as u32);
            }
            character => output.push(character),
        }
    }
    output.push('"');
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_requires_a_hardware_stamp() {
        let error = parse_multiplayer_profile_args(["--samples", "100"]).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("--hardware is required and cannot be empty")
        );
    }

    #[test]
    fn parser_applies_defaults_and_accepts_report_only() {
        let MultiplayerProfileCliAction::Run(config) =
            parse_multiplayer_profile_args(["--hardware", "Apple M2 Max / macOS", "--report-only"])
                .unwrap()
        else {
            panic!("expected run action");
        };
        assert_eq!(config.hardware, "Apple M2 Max / macOS");
        assert_eq!(config.run_id, "unlabeled");
        assert_eq!(config.seed, DEFAULT_REPLAY_SEED);
        assert_eq!(
            config.authority_warmup_ticks,
            DEFAULT_AUTHORITY_WARMUP_TICKS
        );
        assert_eq!(
            config.rollback_warmup_bursts,
            DEFAULT_ROLLBACK_WARMUP_BURSTS
        );
        assert_eq!(config.samples, DEFAULT_SAMPLE_COUNT);
        assert!(!config.allocation_breakdown);
        assert!(config.report_only);
    }

    #[test]
    fn parser_accepts_explicit_bounded_configuration_and_hex_seed() {
        let MultiplayerProfileCliAction::Run(config) = parse_multiplayer_profile_args([
            "--hardware",
            "minimum CPU",
            "--run-id",
            "commit-run-2",
            "--seed",
            "0x1234abcd",
            "--authority-warmup-ticks",
            "128",
            "--rollback-warmup-bursts",
            "4",
            "--samples",
            "250",
            "--allocation-breakdown",
        ])
        .unwrap() else {
            panic!("expected run action");
        };
        assert_eq!(config.run_id, "commit-run-2");
        assert_eq!(config.seed, 0x1234_abcd);
        assert_eq!(config.authority_warmup_ticks, 128);
        assert_eq!(config.rollback_warmup_bursts, 4);
        assert_eq!(config.samples, 250);
        assert!(config.allocation_breakdown);
        assert!(!config.report_only);
    }

    #[test]
    fn parser_rejects_ambiguous_or_statistically_tiny_runs() {
        assert!(
            parse_multiplayer_profile_args(["--hardware", "cpu", "--hardware", "other cpu",])
                .is_err()
        );
        assert!(parse_multiplayer_profile_args(["--hardware", "cpu", "--samples", "99",]).is_err());
        assert!(parse_multiplayer_profile_args(["--hardware", "cpu", "--unknown"]).is_err());
        assert!(
            parse_multiplayer_profile_args([
                "--hardware",
                "cpu",
                "--allocation-breakdown",
                "--allocation-breakdown",
            ])
            .is_err()
        );
    }

    #[test]
    fn nearest_rank_percentiles_are_stable() {
        let mut values = (1_u64..=100).collect::<Vec<_>>();
        let distribution = TimingDistribution::from_samples(&mut values);
        assert_eq!(distribution.p50_ns, 50);
        assert_eq!(distribution.p95_ns, 95);
        assert_eq!(distribution.p99_ns, 99);
        assert_eq!(distribution.maximum_ns, 100);
    }

    #[test]
    fn machine_record_escapes_labels_and_exposes_acceptance_fields() {
        let result = MultiplayerProfileResult {
            hardware: "CPU \"A\"\nmacOS".to_owned(),
            run_id: "run\\1".to_owned(),
            seed: 7,
            authority_warmup_ticks: 2,
            rollback_warmup_bursts: 3,
            authority: AuthorityProfileResult {
                timing: TimingDistribution {
                    samples: 100,
                    p50_ns: 10,
                    p95_ns: 20,
                    p99_ns: 30,
                    maximum_ns: 40,
                },
                timing_pass: true,
                steady_state_allocation_pass: true,
                history_pass: true,
                pass: true,
                ..AuthorityProfileResult::default()
            },
            rollback: RollbackProfileResult {
                timing: TimingDistribution {
                    samples: 100,
                    p50_ns: 50,
                    p95_ns: 60,
                    p99_ns: 70,
                    maximum_ns: 80,
                },
                requested_depth_ticks: 12,
                maximum_depth_ticks: 12,
                configured_depth_cap_ticks: 12,
                timing_pass: true,
                depth_pass: true,
                history_pass: true,
                pass: true,
                ..RollbackProfileResult::default()
            },
            acceptance_pass: true,
        };
        let record = result.machine_record();
        assert!(record.starts_with("AFC_MULTIPLAYER_PERF_RESULT {"));
        assert!(record.contains("\"hardware\":\"CPU \\\"A\\\"\\nmacOS\""));
        assert!(record.contains("\"run_id\":\"run\\\\1\""));
        assert!(record.contains("\"steady_state_allocation_pass\":true"));
        assert!(record.ends_with("\"acceptance_pass\":true}"));
    }
}
