//! Deployment/test composition root for a render-free dedicated authority.
//!
//! The first-release binary is an all-bot local smoke harness. This module
//! deliberately stops at the authority command boundary and does not provide a
//! Steam GameServer login, hosted SDR listener, player admission, ranked mode,
//! or trusted result submission. Constructing or exercising this authority is
//! therefore not evidence that hosted Steam dedicated play is enabled.

use std::error::Error;
use std::fmt;
use std::num::NonZeroU32;
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::authority::{AuthorityMatch, AuthorityMatchError};
use crate::authority_input::AuthorityInputConfig;
use crate::authority_thread::{
    AuthorityThreadConfig, AuthorityThreadExit, AuthorityThreadHandle, AuthorityThreadJoinError,
    AuthorityThreadSpawnError, AuthorityThreadTerminal, spawn_authority_thread_from_factory,
};
use crate::components::{LocalInputAssignment, ParticipantKind};
use crate::game_state::LocalSetup;
use crate::headless::{HeadlessBuildError, HeadlessMatchConfig, build_headless_simulation};
use crate::live_authority::{LiveSimulationDriver, LiveSimulationError};
use crate::match_config::{MatchBuildOptions, MatchConfigError, build_headless_match_config};
use crate::network_protocol::{AuthorityKind, MAX_FIGHTERS, MatchId, SimTick};
use crate::release_identity::{CommonReleaseCliAction, common_release_cli_action};

const DEFAULT_AGREED_START_TICK: SimTick = SimTick(120);
const SERVER_POLL_INTERVAL: Duration = Duration::from_millis(50);
const SMOKE_WATCHDOG_BASE: Duration = Duration::from_secs(5);
const SMOKE_WATCHDOG_PER_TICK: Duration = Duration::from_millis(50);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DedicatedDeploymentScope {
    /// Render-free deterministic authority exercised only by local/deployment
    /// automation with authority-owned bot seats.
    LocalBotSmokeOnly,
}

pub const FIRST_RELEASE_DEDICATED_SCOPE: DedicatedDeploymentScope =
    DedicatedDeploymentScope::LocalBotSmokeOnly;
pub const HOSTED_STEAM_DEDICATED_ENABLED: bool = false;
pub const TRUSTED_DEDICATED_RESULTS_ENABLED: bool = false;

/// Standalone all-bot settings for deployment smoke tests. This does not
/// represent a player-facing or lobby-backed server capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DedicatedLaunchOptions {
    pub match_id: MatchId,
    pub master_seed: u64,
    pub arena_index: usize,
    pub rule_index: usize,
    pub bot_fighters: u8,
    pub smoke_ticks: Option<NonZeroU32>,
}

impl DedicatedLaunchOptions {
    pub const fn new(match_id: MatchId) -> Self {
        Self {
            match_id,
            master_seed: crate::game_state::DEFAULT_REPLAY_SEED,
            arena_index: 0,
            rule_index: 0,
            bot_fighters: 2,
            smoke_ticks: None,
        }
    }

    /// Builds an explicitly untrusted all-bot manifest for the local smoke
    /// harness. A future hosted service must supply its own authenticated,
    /// policy-approved manifest instead of promoting this helper.
    pub fn headless_config(self) -> Result<HeadlessMatchConfig, DedicatedServerError> {
        if !(2..=MAX_FIGHTERS as u8).contains(&self.bot_fighters) {
            return Err(DedicatedServerError::InvalidBotFighterCount {
                value: self.bot_fighters,
                max: MAX_FIGHTERS as u8,
            });
        }

        let mut setup = LocalSetup::default();
        setup.arena_index = self.arena_index;
        setup.rule_index = self.rule_index;
        setup.replay_seed = self.master_seed;
        for (index, slot) in setup.slots.iter_mut().enumerate() {
            slot.participant = if index < usize::from(self.bot_fighters) {
                ParticipantKind::Bot
            } else {
                ParticipantKind::Closed
            };
            slot.input = LocalInputAssignment::Unassigned;
        }

        let build = MatchBuildOptions {
            match_id: self.match_id,
            authority: AuthorityKind::Dedicated,
            trusted_results: false,
            human_owners: [None; MAX_FIGHTERS],
            agreed_start_tick: DEFAULT_AGREED_START_TICK,
            input_delay_ticks: crate::match_config::DEFAULT_INPUT_DELAY_TICKS,
            rollback_limit_ticks: crate::match_config::DEFAULT_ROLLBACK_LIMIT_TICKS,
            snapshot_history_ticks: crate::match_config::DEFAULT_SNAPSHOT_HISTORY_TICKS,
        };
        build_headless_match_config(&setup, build).map_err(Into::into)
    }
}

#[derive(Debug)]
pub enum DedicatedServerError {
    Arguments(String),
    InvalidBotFighterCount {
        value: u8,
        max: u8,
    },
    MatchConfig(MatchConfigError),
    HeadlessBuild(HeadlessBuildError),
    AuthorityBuild(AuthorityMatchError<LiveSimulationError>),
    ThreadSpawn(AuthorityThreadSpawnError),
    ThreadJoin(AuthorityThreadJoinError),
    AuthorityRuntime(AuthorityMatchError<LiveSimulationError>),
    BootstrapRuntime(String),
    CommandChannelDisconnected,
    TerminalChannelDisconnected,
    SmokeWatchdog {
        requested_ticks: u32,
        last_tick: SimTick,
    },
}

impl fmt::Display for DedicatedServerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Arguments(message) => {
                write!(formatter, "invalid dedicated-server arguments: {message}")
            }
            Self::InvalidBotFighterCount { value, max } => write!(
                formatter,
                "dedicated bot fighter count {value} is outside the supported range 2..={max}"
            ),
            Self::MatchConfig(error) => error.fmt(formatter),
            Self::HeadlessBuild(error) => error.fmt(formatter),
            Self::AuthorityBuild(error) => {
                write!(
                    formatter,
                    "dedicated authority construction failed: {error:?}"
                )
            }
            Self::ThreadSpawn(error) => error.fmt(formatter),
            Self::ThreadJoin(error) => error.fmt(formatter),
            Self::AuthorityRuntime(error) => {
                write!(
                    formatter,
                    "dedicated authority terminated with an error: {error:?}"
                )
            }
            Self::BootstrapRuntime(error) => {
                write!(
                    formatter,
                    "dedicated authority bootstrap failed on its worker: {error}"
                )
            }
            Self::CommandChannelDisconnected => {
                write!(
                    formatter,
                    "dedicated authority command channel disconnected"
                )
            }
            Self::TerminalChannelDisconnected => {
                write!(
                    formatter,
                    "dedicated authority terminal channel disconnected"
                )
            }
            Self::SmokeWatchdog {
                requested_ticks,
                last_tick,
            } => write!(
                formatter,
                "dedicated smoke run failed to reach {requested_ticks} ticks; last tick was {}",
                last_tick.get()
            ),
        }
    }
}

impl Error for DedicatedServerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::MatchConfig(error) => Some(error),
            Self::HeadlessBuild(error) => Some(error),
            Self::ThreadSpawn(error) => Some(error),
            Self::ThreadJoin(error) => Some(error),
            _ => None,
        }
    }
}

impl From<MatchConfigError> for DedicatedServerError {
    fn from(error: MatchConfigError) -> Self {
        Self::MatchConfig(error)
    }
}

impl From<HeadlessBuildError> for DedicatedServerError {
    fn from(error: HeadlessBuildError) -> Self {
        Self::HeadlessBuild(error)
    }
}

impl From<AuthorityThreadSpawnError> for DedicatedServerError {
    fn from(error: AuthorityThreadSpawnError) -> Self {
        Self::ThreadSpawn(error)
    }
}

impl From<AuthorityThreadJoinError> for DedicatedServerError {
    fn from(error: AuthorityThreadJoinError) -> Self {
        Self::ThreadJoin(error)
    }
}

/// Constructs the authoritative world and captures its initial validated
/// snapshot before returning. No worker or transport is started on failure.
pub fn build_dedicated_authority(
    config: HeadlessMatchConfig,
    input_config: AuthorityInputConfig,
) -> Result<AuthorityMatch<LiveSimulationDriver>, DedicatedServerError> {
    let manifest = config.manifest;
    let simulation = build_headless_simulation(config)?;
    AuthorityMatch::new(manifest, simulation, input_config)
        .map_err(DedicatedServerError::AuthorityBuild)
}

/// Starts the transport-agnostic 60 Hz authority worker. This primitive does
/// not enable or attest to a hosted Steam deployment on its own.
pub fn spawn_dedicated_authority(
    config: HeadlessMatchConfig,
    input_config: AuthorityInputConfig,
    thread_config: AuthorityThreadConfig,
) -> Result<AuthorityThreadHandle<LiveSimulationError>, DedicatedServerError> {
    // Validate malformed lobby data synchronously. The Bevy App itself is
    // constructed by the factory because it is thread-affine and cannot be
    // built here and then moved onto the authority worker.
    config.validate()?;
    let manifest_hz = config.manifest.tick_rate_hz;
    spawn_authority_thread_from_factory(manifest_hz, thread_config, move || {
        build_dedicated_authority(config, input_config)
    })
    .map_err(Into::into)
}

/// Runs a standalone bot authority until the match finishes, or until the
/// requested smoke interval has been observed on the real-time worker.
pub fn run_standalone_dedicated(
    options: DedicatedLaunchOptions,
) -> Result<AuthorityThreadTerminal<LiveSimulationError>, DedicatedServerError> {
    let smoke_ticks = options.smoke_ticks;
    let config = options.headless_config()?;
    let initial_tick = SimTick::ZERO;
    let mut handle = spawn_dedicated_authority(
        config,
        AuthorityInputConfig::default(),
        AuthorityThreadConfig::default(),
    )?;
    let started = Instant::now();
    let mut last_tick = initial_tick;
    let smoke_deadline = smoke_ticks.map(|ticks| {
        started + SMOKE_WATCHDOG_BASE + SMOKE_WATCHDOG_PER_TICK.saturating_mul(ticks.get())
    });

    loop {
        match handle.wait_for_terminal(Duration::ZERO) {
            Ok(terminal) => {
                handle.join()?;
                return classify_terminal(terminal, smoke_ticks);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                handle.request_shutdown();
                handle.join()?;
                return Err(DedicatedServerError::TerminalChannelDisconnected);
            }
        }

        match handle
            .reports_mut()
            .recv_latest_timeout(SERVER_POLL_INTERVAL)
        {
            Ok(report) => last_tick = report.tick,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                // Terminal publication is separate and reliable; observe it on
                // the next loop before classifying a report-channel shutdown.
            }
        }

        if let Some(requested) = smoke_ticks
            && last_tick.get().saturating_sub(initial_tick.get()) >= u64::from(requested.get())
        {
            let terminal = handle.shutdown()?;
            return classify_terminal(terminal, smoke_ticks);
        }

        if let Some(deadline) = smoke_deadline
            && Instant::now() >= deadline
        {
            handle.request_shutdown();
            handle.join()?;
            return Err(DedicatedServerError::SmokeWatchdog {
                requested_ticks: smoke_ticks.expect("a smoke deadline has a limit").get(),
                last_tick,
            });
        }
    }
}

fn classify_terminal(
    terminal: AuthorityThreadTerminal<LiveSimulationError>,
    smoke_ticks: Option<NonZeroU32>,
) -> Result<AuthorityThreadTerminal<LiveSimulationError>, DedicatedServerError> {
    match terminal.exit {
        AuthorityThreadExit::MatchFinished { .. } => Ok(terminal),
        AuthorityThreadExit::StopRequested if smoke_ticks.is_some() => Ok(terminal),
        AuthorityThreadExit::AuthorityError(error) => {
            Err(DedicatedServerError::AuthorityRuntime(error))
        }
        AuthorityThreadExit::BootstrapError(error) => {
            Err(DedicatedServerError::BootstrapRuntime(error))
        }
        AuthorityThreadExit::CommandChannelDisconnected => {
            Err(DedicatedServerError::CommandChannelDisconnected)
        }
        AuthorityThreadExit::StopRequested => Err(DedicatedServerError::CommandChannelDisconnected),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DedicatedCliAction {
    Help,
    Version,
    ReleaseIdentity,
    Run(DedicatedLaunchOptions),
}

pub const DEDICATED_HELP: &str = "Animal Fighter Club dedicated deployment/test smoke authority\n\
Hosted Steam SDR, player admission, ranked play, and trusted results are NOT enabled.\n\n\
Usage: afc-dedicated [OPTIONS]\n\n\
Options:\n\
  --version          Print the product and compatibility version\n\
  --release-identity Print deterministic release identity JSON\n\
  --smoke-ticks N    Stop cleanly after observing N real-time authority ticks\n\
  --seed N           Master deterministic gameplay seed (decimal or 0xHEX)\n\
  --arena N          Arena definition index\n\
  --rules N          Rules definition index\n\
  --fighters N       Authority-bot fighters (2..=4, default 2)\n\
  --match-id HEX     Exact 16-byte match ID encoded as 32 hex digits\n\
  -h, --help         Print this help\n";

pub fn parse_dedicated_args<I, S>(args: I) -> Result<DedicatedCliAction, DedicatedServerError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut options = DedicatedLaunchOptions::new(generated_match_id());
    let args = args.into_iter().map(Into::into).collect::<Vec<String>>();
    match common_release_cli_action(args.iter().map(String::as_str)) {
        CommonReleaseCliAction::Version => return Ok(DedicatedCliAction::Version),
        CommonReleaseCliAction::ReleaseIdentity => {
            return Ok(DedicatedCliAction::ReleaseIdentity);
        }
        CommonReleaseCliAction::Run => {}
    }

    let mut args = args.into_iter();
    while let Some(argument) = args.next() {
        if argument == "-h" || argument == "--help" {
            return Ok(DedicatedCliAction::Help);
        }
        let value = args.next().ok_or_else(|| {
            DedicatedServerError::Arguments(format!("missing value for {argument}"))
        })?;
        match argument.as_str() {
            "--smoke-ticks" => {
                let value = parse_u32(&argument, &value)?;
                options.smoke_ticks = Some(NonZeroU32::new(value).ok_or_else(|| {
                    DedicatedServerError::Arguments("--smoke-ticks must be non-zero".to_owned())
                })?);
            }
            "--seed" => options.master_seed = parse_u64(&argument, &value)?,
            "--arena" => options.arena_index = parse_usize(&argument, &value)?,
            "--rules" => options.rule_index = parse_usize(&argument, &value)?,
            "--fighters" => {
                options.bot_fighters = value.parse::<u8>().map_err(|_| {
                    DedicatedServerError::Arguments(format!(
                        "{argument} expects an unsigned integer, got '{value}'"
                    ))
                })?;
            }
            "--match-id" => options.match_id = parse_match_id(&value)?,
            _ => {
                return Err(DedicatedServerError::Arguments(format!(
                    "unknown option '{argument}'"
                )));
            }
        }
    }
    Ok(DedicatedCliAction::Run(options))
}

fn parse_u32(option: &str, value: &str) -> Result<u32, DedicatedServerError> {
    value.parse().map_err(|_| {
        DedicatedServerError::Arguments(format!(
            "{option} expects an unsigned integer, got '{value}'"
        ))
    })
}

fn parse_usize(option: &str, value: &str) -> Result<usize, DedicatedServerError> {
    value.parse().map_err(|_| {
        DedicatedServerError::Arguments(format!(
            "{option} expects an unsigned integer, got '{value}'"
        ))
    })
}

fn parse_u64(option: &str, value: &str) -> Result<u64, DedicatedServerError> {
    let parsed = value
        .strip_prefix("0x")
        .map(|hex| u64::from_str_radix(hex, 16))
        .unwrap_or_else(|| value.parse());
    parsed.map_err(|_| {
        DedicatedServerError::Arguments(format!(
            "{option} expects a decimal integer or 0x-prefixed hex, got '{value}'"
        ))
    })
}

fn parse_match_id(value: &str) -> Result<MatchId, DedicatedServerError> {
    if value.len() != 32 || !value.is_ascii() {
        return Err(DedicatedServerError::Arguments(
            "--match-id requires exactly 32 hex digits".to_owned(),
        ));
    }
    let mut bytes = [0_u8; 16];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).map_err(|_| {
            DedicatedServerError::Arguments("--match-id requires exactly 32 hex digits".to_owned())
        })?;
    }
    MatchId::new(bytes).map_err(|error| {
        DedicatedServerError::Arguments(format!("--match-id is invalid: {error:?}"))
    })
}

fn generated_match_id() -> MatchId {
    let mut bytes = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .to_le_bytes();
    if bytes == [0; 16] {
        bytes[0] = 1;
    }
    MatchId::new(bytes).expect("the generated standalone match ID is non-zero")
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::asset::AssetServer;
    use bevy::audio::AudioSource;
    use bevy::prelude::{Assets, Component, Image, World};
    use bevy::window::PrimaryWindow;

    use crate::arena_defs::BUMPER_ALLEY_ARENA_INDEX;
    use crate::bee_skills::ActiveBeeSkill;
    use crate::chick_skills::ActiveChickSkill;
    use crate::components::Hitbox;
    use crate::penguin_skills::{ActivePenguinSkill, ActivePenguinSurface};
    use crate::sim_event::{SimEventJournal, SimEventKind, TickEventBuffer};
    use crate::specials::ActiveSpecial;

    fn match_id() -> MatchId {
        MatchId::new(*b"dedicated-test-1").unwrap()
    }

    fn component_count<T: Component>(world: &World) -> usize {
        world
            .archetypes()
            .iter()
            .flat_map(|archetype| archetype.entities())
            .filter(|entry| world.get::<T>(entry.id()).is_some())
            .count()
    }

    fn add_owner_work<T: Component>(
        world: &World,
        workload: &mut [usize; MAX_FIGHTERS],
        owner: impl Fn(&T) -> usize,
    ) {
        for archetype in world.archetypes().iter() {
            for entry in archetype.entities() {
                if let Some(component) = world.get::<T>(entry.id()) {
                    let owner = owner(component);
                    if owner < MAX_FIGHTERS {
                        workload[owner] += 1;
                    }
                }
            }
        }
    }

    fn owner_workload(world: &World) -> [usize; MAX_FIGHTERS] {
        let mut workload = [0; MAX_FIGHTERS];
        add_owner_work::<Hitbox>(world, &mut workload, |hitbox| hitbox.owner.index());
        add_owner_work::<ActiveSpecial>(world, &mut workload, |special| special.owner.index());
        add_owner_work::<ActiveBeeSkill>(world, &mut workload, |skill| skill.owner.index());
        add_owner_work::<ActiveChickSkill>(world, &mut workload, |skill| skill.owner.index());
        add_owner_work::<ActivePenguinSkill>(world, &mut workload, |skill| skill.owner.index());
        add_owner_work::<ActivePenguinSurface>(world, &mut workload, |surface| {
            surface.owner.index()
        });
        workload
    }

    #[test]
    fn standalone_config_is_untrusted_dedicated_all_bot_and_render_free() {
        let options = DedicatedLaunchOptions::new(match_id());
        let config = options.headless_config().unwrap();
        assert_eq!(config.manifest.authority, AuthorityKind::Dedicated);
        assert!(!config.manifest.trusted_results);
        assert_eq!(
            FIRST_RELEASE_DEDICATED_SCOPE,
            DedicatedDeploymentScope::LocalBotSmokeOnly
        );
        assert!(!HOSTED_STEAM_DEDICATED_ENABLED);
        assert!(!TRUSTED_DEDICATED_RESULTS_ENABLED);
        assert!(
            config
                .manifest
                .ownership
                .as_slice()
                .iter()
                .all(|assignment| assignment.owner
                    == crate::network_protocol::SeatOwner::AuthorityBot)
        );

        let authority = build_dedicated_authority(config, AuthorityInputConfig::default()).unwrap();
        let world = authority.simulation().world();
        assert!(!world.contains_resource::<AssetServer>());
        assert!(!world.contains_resource::<Assets<Image>>());
        assert!(!world.contains_resource::<Assets<AudioSource>>());
        assert_eq!(component_count::<PrimaryWindow>(world), 0);
    }

    #[test]
    fn four_production_bots_all_attack_hit_and_create_owner_workload() {
        const MAX_TICKS: u32 = 1_800;

        let options = DedicatedLaunchOptions {
            match_id: MatchId::new(*b"4bot-bumper-test").unwrap(),
            master_seed: 0x0000_0000_FFC0_0001,
            arena_index: BUMPER_ALLEY_ARENA_INDEX,
            rule_index: 1,
            bot_fighters: MAX_FIGHTERS as u8,
            smoke_ticks: None,
        };
        let config = options.headless_config().unwrap();
        assert_eq!(config.manifest.ownership.len(), MAX_FIGHTERS);
        assert!(
            config
                .manifest
                .ownership
                .as_slice()
                .iter()
                .all(|assignment| assignment.owner
                    == crate::network_protocol::SeatOwner::AuthorityBot)
        );
        let mut authority =
            build_dedicated_authority(config, AuthorityInputConfig::default()).unwrap();
        let mut actions = [0_usize; MAX_FIGHTERS];
        let mut hits = [0_usize; MAX_FIGHTERS];
        let mut workload_peak = [0_usize; MAX_FIGHTERS];

        for _ in 0..MAX_TICKS {
            let report = authority.step().unwrap();
            let world = authority.simulation().world();
            for event in world.resource::<SimEventJournal>().iter_at(report.tick) {
                match event.kind {
                    SimEventKind::ActionStarted { fighter, .. } => {
                        actions[fighter.index()] += 1;
                    }
                    SimEventKind::HitConfirmed {
                        attacker: Some(attacker),
                        ..
                    } => {
                        hits[attacker.index()] += 1;
                    }
                    _ => {}
                }
            }
            for (peak, current) in workload_peak.iter_mut().zip(owner_workload(world)) {
                *peak = (*peak).max(current);
            }
            if actions.iter().all(|count| *count > 0)
                && hits.iter().all(|count| *count > 0)
                && workload_peak.iter().all(|count| *count > 0)
            {
                break;
            }
        }

        assert!(
            actions.iter().all(|count| *count > 0),
            "every production bot must emit ActionStarted within {MAX_TICKS} ticks: {actions:?}"
        );
        assert!(
            hits.iter().all(|count| *count > 0),
            "every production bot must emit HitConfirmed within {MAX_TICKS} ticks: {hits:?}"
        );
        assert!(
            workload_peak.iter().all(|count| *count > 0),
            "every production bot must create live owner workload within {MAX_TICKS} ticks: \
             {workload_peak:?}"
        );
        assert_eq!(
            authority
                .simulation()
                .world()
                .resource::<TickEventBuffer>()
                .overflow_count(),
            0,
            "the bounded activity proof must not lose canonical events"
        );
    }

    #[test]
    fn smoke_mode_uses_real_time_authority_thread_and_stops_cleanly() {
        let mut options = DedicatedLaunchOptions::new(match_id());
        options.smoke_ticks = NonZeroU32::new(2);
        let terminal = run_standalone_dedicated(options).unwrap();

        assert!(matches!(
            terminal.exit,
            AuthorityThreadExit::StopRequested | AuthorityThreadExit::MatchFinished { .. }
        ));
        assert!(terminal.last_tick.get() >= 2);
        assert!(terminal.metrics.simulated_ticks >= 2);
    }

    #[test]
    fn cli_is_strict_and_builds_the_requested_manifest_inputs() {
        let action = parse_dedicated_args([
            "--smoke-ticks",
            "3",
            "--seed",
            "0x2a",
            "--arena",
            "1",
            "--rules",
            "0",
            "--fighters",
            "4",
            "--match-id",
            "0102030405060708090a0b0c0d0e0f10",
        ])
        .unwrap();
        let DedicatedCliAction::Run(options) = action else {
            panic!("expected run action");
        };
        assert_eq!(options.smoke_ticks.unwrap().get(), 3);
        assert_eq!(options.master_seed, 42);
        assert_eq!(options.arena_index, 1);
        assert_eq!(options.bot_fighters, 4);
        options.headless_config().unwrap().validate().unwrap();

        assert!(parse_dedicated_args(["--smoke-ticks", "0"]).is_err());
        assert!(parse_dedicated_args(["--unknown", "1"]).is_err());
        assert!(parse_dedicated_args(["--match-id", "00"]).is_err());
    }

    #[test]
    fn cli_release_diagnostics_are_exact_and_standalone_only() {
        assert_eq!(
            parse_dedicated_args(["--version"]).unwrap(),
            DedicatedCliAction::Version
        );
        assert_eq!(
            parse_dedicated_args(["--release-identity"]).unwrap(),
            DedicatedCliAction::ReleaseIdentity
        );
        assert!(parse_dedicated_args(["--version", "--smoke-ticks", "1"]).is_err());
        assert!(parse_dedicated_args(["--release-identity", "extra"]).is_err());
    }
}
