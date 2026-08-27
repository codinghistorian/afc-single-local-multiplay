use std::sync::mpsc::{self, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use tokio::sync::oneshot;

use crate::authority::AuthoritySimulation;
use crate::authority_input::AuthorityInputConfig;
use crate::authority_peer_hub::{AuthorityConnectionId, AuthorityPeerHub, AuthorityPeerHubConfig};
use crate::headless::{HeadlessMatchConfig, build_headless_simulation};
use crate::listen_authority::deterministic_disconnected_bot_frame;
use crate::live_authority::LiveSimulationDriver;
use crate::multiplayer_observability::ServerTickDistribution;
use crate::network_codec::ResultIdentifier;
use crate::network_protocol::{PeerId, ReconnectClaim, SimTick};
use crate::reconnect::{AuthenticatedPeer, AuthenticatedUserId};
use crate::web_endpoint_adapters::ServerDatagramEndpoint;

const NANOS_PER_SECOND: u128 = 1_000_000_000;
const AUTHORITY_HZ: u128 = 60;
const MAX_COMMAND_CAPACITY: usize = 1_024;
const MIN_STARTUP_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_STARTUP_TIMEOUT: Duration = Duration::from_secs(30);

type HostedAuthorityHub = AuthorityPeerHub<LiveSimulationDriver, ServerDatagramEndpoint>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WebRoomWorkerConfig {
    pub command_capacity: usize,
    pub startup_timeout: Duration,
    pub authority: AuthorityPeerHubConfig,
    pub input: AuthorityInputConfig,
}

impl Default for WebRoomWorkerConfig {
    fn default() -> Self {
        Self {
            command_capacity: 32,
            startup_timeout: Duration::from_secs(10),
            authority: AuthorityPeerHubConfig::default(),
            input: AuthorityInputConfig::default(),
        }
    }
}

impl WebRoomWorkerConfig {
    pub(super) fn validate(self) -> Result<(), WebRoomWorkerError> {
        if self.command_capacity == 0
            || self.command_capacity > MAX_COMMAND_CAPACITY
            || self.startup_timeout < MIN_STARTUP_TIMEOUT
            || self.startup_timeout > MAX_STARTUP_TIMEOUT
        {
            return Err(WebRoomWorkerError::InvalidConfiguration);
        }
        self.input
            .validate()
            .map_err(|_| WebRoomWorkerError::InvalidConfiguration)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WebRoomWorkerPhase {
    #[default]
    Starting,
    WaitingForPeers,
    Countdown,
    Fighting,
    Finished,
    Draining,
    Stopped,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebRoomWorkerSnapshot {
    pub phase: WebRoomWorkerPhase,
    pub network_tick: SimTick,
    pub simulation_tick: SimTick,
    pub connected_peers: u8,
    /// Bit `n` is set when sealed-roster member `n` has a live hub generation.
    pub connected_peer_mask: u8,
    pub confirmed_result: Option<ResultIdentifier>,
    pub server_ticks: ServerTickDistribution,
    pub error: Option<WebRoomWorkerError>,
}

impl Default for WebRoomWorkerSnapshot {
    fn default() -> Self {
        Self {
            phase: WebRoomWorkerPhase::Starting,
            network_tick: SimTick::ZERO,
            simulation_tick: SimTick::ZERO,
            connected_peers: 0,
            connected_peer_mask: 0,
            confirmed_result: None,
            server_ticks: ServerTickDistribution::default(),
            error: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WebRoomWorkerError {
    InvalidConfiguration,
    InvalidRoster,
    ThreadSpawn,
    StartupTimeout,
    StartupChannelClosed,
    CommandQueueFull,
    WorkerStopped,
    TimelineExhausted,
    Authority(String),
}

impl std::fmt::Display for WebRoomWorkerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "hosted room worker failed: {self:?}")
    }
}

impl std::error::Error for WebRoomWorkerError {}

enum WebRoomCommand {
    AttachInitial {
        peer_id: PeerId,
        user_id: AuthenticatedUserId,
        endpoint: ServerDatagramEndpoint,
        response: oneshot::Sender<Result<AuthorityConnectionId, WebRoomWorkerError>>,
    },
    AttachReconnect {
        user_id: AuthenticatedUserId,
        claim: ReconnectClaim,
        endpoint: ServerDatagramEndpoint,
        response: oneshot::Sender<Result<AuthorityConnectionId, WebRoomWorkerError>>,
    },
    Shutdown {
        response: Option<oneshot::Sender<Result<(), WebRoomWorkerError>>>,
    },
}

struct WebRoomWorkerInner {
    commands: SyncSender<WebRoomCommand>,
    snapshot: Arc<Mutex<WebRoomWorkerSnapshot>>,
    join: Mutex<Option<JoinHandle<()>>>,
}

#[derive(Clone)]
pub(super) struct WebRoomWorkerHandle {
    inner: Arc<WebRoomWorkerInner>,
}

impl WebRoomWorkerHandle {
    pub(super) fn spawn(
        match_config: HeadlessMatchConfig,
        roster: Vec<AuthenticatedPeer>,
        config: WebRoomWorkerConfig,
    ) -> Result<Self, WebRoomWorkerError> {
        config.validate()?;
        if roster.is_empty() || roster.len() > crate::network_protocol::MAX_SEATS {
            return Err(WebRoomWorkerError::InvalidRoster);
        }
        let manifest = match_config.manifest;
        let (command_tx, command_rx) = mpsc::sync_channel(config.command_capacity);
        let (startup_tx, startup_rx) = mpsc::sync_channel(1);
        let snapshot = Arc::new(Mutex::new(WebRoomWorkerSnapshot::default()));
        let worker_snapshot = Arc::clone(&snapshot);
        let thread_name = format!(
            "afc-web-room-{:02x}{:02x}{:02x}{:02x}",
            manifest.match_id.as_bytes()[0],
            manifest.match_id.as_bytes()[1],
            manifest.match_id.as_bytes()[2],
            manifest.match_id.as_bytes()[3]
        );
        let join = thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                let initialized = initialize_hub(match_config, &roster, config);
                match initialized {
                    Ok(mut hub) => {
                        let _ = startup_tx.send(Ok(()));
                        if let Err(error) = run_worker_loop(
                            &mut hub,
                            &roster,
                            command_rx,
                            &worker_snapshot,
                            manifest.master_gameplay_seed,
                        ) {
                            let mut snapshot = lock_recover(&worker_snapshot);
                            snapshot.phase = WebRoomWorkerPhase::Failed;
                            snapshot.error = Some(error);
                        }
                    }
                    Err(error) => {
                        let _ = startup_tx.send(Err(error.clone()));
                        let mut snapshot = lock_recover(&worker_snapshot);
                        snapshot.phase = WebRoomWorkerPhase::Failed;
                        snapshot.error = Some(error);
                    }
                }
            })
            .map_err(|_| WebRoomWorkerError::ThreadSpawn)?;

        match startup_rx.recv_timeout(config.startup_timeout) {
            Ok(Ok(())) => Ok(Self {
                inner: Arc::new(WebRoomWorkerInner {
                    commands: command_tx,
                    snapshot,
                    join: Mutex::new(Some(join)),
                }),
            }),
            Ok(Err(error)) => {
                let _ = join.join();
                Err(error)
            }
            Err(RecvTimeoutError::Timeout) => Err(WebRoomWorkerError::StartupTimeout),
            Err(RecvTimeoutError::Disconnected) => {
                let _ = join.join();
                Err(WebRoomWorkerError::StartupChannelClosed)
            }
        }
    }

    pub(super) fn snapshot(&self) -> WebRoomWorkerSnapshot {
        lock_recover(&self.inner.snapshot).clone()
    }

    pub(super) async fn attach_initial(
        &self,
        peer_id: PeerId,
        user_id: AuthenticatedUserId,
        endpoint: ServerDatagramEndpoint,
    ) -> Result<AuthorityConnectionId, WebRoomWorkerError> {
        let (response_tx, response_rx) = oneshot::channel();
        self.try_send(WebRoomCommand::AttachInitial {
            peer_id,
            user_id,
            endpoint,
            response: response_tx,
        })?;
        response_rx
            .await
            .map_err(|_| WebRoomWorkerError::WorkerStopped)?
    }

    pub(super) async fn attach_reconnect(
        &self,
        user_id: AuthenticatedUserId,
        claim: ReconnectClaim,
        endpoint: ServerDatagramEndpoint,
    ) -> Result<AuthorityConnectionId, WebRoomWorkerError> {
        let (response_tx, response_rx) = oneshot::channel();
        self.try_send(WebRoomCommand::AttachReconnect {
            user_id,
            claim,
            endpoint,
            response: response_tx,
        })?;
        response_rx
            .await
            .map_err(|_| WebRoomWorkerError::WorkerStopped)?
    }

    pub(super) fn request_shutdown(&self) -> Result<(), WebRoomWorkerError> {
        self.try_send(WebRoomCommand::Shutdown { response: None })
    }

    pub(super) async fn shutdown(&self) -> Result<(), WebRoomWorkerError> {
        let (response_tx, response_rx) = oneshot::channel();
        self.try_send(WebRoomCommand::Shutdown {
            response: Some(response_tx),
        })?;
        response_rx
            .await
            .map_err(|_| WebRoomWorkerError::WorkerStopped)??;
        Ok(())
    }

    pub(super) fn join_if_stopped(&self) {
        if !matches!(
            self.snapshot().phase,
            WebRoomWorkerPhase::Stopped | WebRoomWorkerPhase::Failed
        ) {
            return;
        }
        if let Some(join) = lock_recover(&self.inner.join).take() {
            let _ = join.join();
        }
    }

    fn try_send(&self, command: WebRoomCommand) -> Result<(), WebRoomWorkerError> {
        match self.inner.commands.try_send(command) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(WebRoomWorkerError::CommandQueueFull),
            Err(TrySendError::Disconnected(_)) => Err(WebRoomWorkerError::WorkerStopped),
        }
    }
}

fn initialize_hub(
    match_config: HeadlessMatchConfig,
    roster: &[AuthenticatedPeer],
    config: WebRoomWorkerConfig,
) -> Result<HostedAuthorityHub, WebRoomWorkerError> {
    let manifest = match_config.manifest;
    let simulation = build_headless_simulation(match_config)
        .map_err(|error| WebRoomWorkerError::Authority(format!("{error:?}")))?;
    AuthorityPeerHub::new(manifest, simulation, config.input, roster, config.authority)
        .map_err(|error| WebRoomWorkerError::Authority(format!("{error:?}")))
}

fn run_worker_loop(
    hub: &mut HostedAuthorityHub,
    roster: &[AuthenticatedPeer],
    commands: mpsc::Receiver<WebRoomCommand>,
    snapshot: &Arc<Mutex<WebRoomWorkerSnapshot>>,
    match_seed: u64,
) -> Result<(), WebRoomWorkerError> {
    let epoch = Instant::now();
    let mut next_network_tick = 1_u64;
    let mut commands_open = true;
    let mut shutting_down = false;
    let mut consecutive_catch_up_ticks = 0_u8;

    loop {
        if shutting_down && hub.shutdown_drained() {
            let mut published = lock_recover(snapshot);
            publish_snapshot(hub, roster, &mut published);
            published.phase = WebRoomWorkerPhase::Stopped;
            return Ok(());
        }

        let deadline = authority_deadline(epoch, next_network_tick)?;
        if commands_open {
            match commands.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(command) => {
                    handle_command(hub, command, &mut shutting_down);
                    continue;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    commands_open = false;
                    begin_shutdown(hub, &mut shutting_down, None);
                }
            }
        } else {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if !remaining.is_zero() {
                thread::sleep(remaining);
            }
        }

        let service_started = Instant::now();
        let network_tick = SimTick(next_network_tick);
        let monotonic_ms = u64::try_from(epoch.elapsed().as_millis()).unwrap_or(u64::MAX);
        hub.pump_network_at(network_tick, monotonic_ms)
            .map_err(|error| WebRoomWorkerError::Authority(format!("{error:?}")))?;
        let _ = hub
            .try_advance(|peer, seat, tick| {
                deterministic_disconnected_bot_frame(match_seed, peer, seat, tick)
            })
            .map_err(|error| WebRoomWorkerError::Authority(format!("{error:?}")))?;
        hub.observe_server_tick(
            u64::try_from(service_started.elapsed().as_nanos()).unwrap_or(u64::MAX),
        );
        while hub.try_next_drain_event().is_some() {}

        {
            let mut published = lock_recover(snapshot);
            publish_snapshot(hub, roster, &mut published);
            if shutting_down {
                published.phase = WebRoomWorkerPhase::Draining;
            }
        }

        next_network_tick = next_network_tick
            .checked_add(1)
            .ok_or(WebRoomWorkerError::TimelineExhausted)?;
        if Instant::now() >= authority_deadline(epoch, next_network_tick)? {
            consecutive_catch_up_ticks = consecutive_catch_up_ticks.saturating_add(1);
            if consecutive_catch_up_ticks >= 8 {
                thread::yield_now();
                consecutive_catch_up_ticks = 0;
            }
        } else {
            consecutive_catch_up_ticks = 0;
        }
    }
}

fn handle_command(hub: &mut HostedAuthorityHub, command: WebRoomCommand, shutting_down: &mut bool) {
    match command {
        WebRoomCommand::AttachInitial {
            peer_id,
            user_id,
            endpoint,
            response,
        } => {
            let result = if *shutting_down {
                Err(WebRoomWorkerError::WorkerStopped)
            } else {
                hub.attach_initial(peer_id, user_id, endpoint)
                    .map_err(|error| WebRoomWorkerError::Authority(format!("{error:?}")))
            };
            let _ = response.send(result);
        }
        WebRoomCommand::AttachReconnect {
            user_id,
            claim,
            endpoint,
            response,
        } => {
            let result = if *shutting_down {
                Err(WebRoomWorkerError::WorkerStopped)
            } else {
                hub.attach_reconnect(user_id, claim, endpoint)
                    .map_err(|error| WebRoomWorkerError::Authority(format!("{error:?}")))
            };
            let _ = response.send(result);
        }
        WebRoomCommand::Shutdown { response } => {
            begin_shutdown(hub, shutting_down, response);
        }
    }
}

fn begin_shutdown(
    hub: &mut HostedAuthorityHub,
    shutting_down: &mut bool,
    response: Option<oneshot::Sender<Result<(), WebRoomWorkerError>>>,
) {
    let result = hub
        .begin_dedicated_shutdown()
        .map_err(|error| WebRoomWorkerError::Authority(format!("{error:?}")));
    if result.is_ok() {
        *shutting_down = true;
    }
    if let Some(response) = response {
        let _ = response.send(result);
    }
}

fn publish_snapshot(
    hub: &HostedAuthorityHub,
    roster: &[AuthenticatedPeer],
    published: &mut WebRoomWorkerSnapshot,
) {
    published.network_tick = hub.network_tick();
    published.simulation_tick = hub.authority().simulation().current_tick();
    let mut connected_peer_mask = 0_u8;
    for (index, peer) in roster.iter().enumerate() {
        if hub.connection_for_peer(peer.peer_id).is_some() {
            connected_peer_mask |= 1_u8 << index;
        }
    }
    published.connected_peer_mask = connected_peer_mask;
    published.connected_peers = connected_peer_mask.count_ones() as u8;
    published.confirmed_result = hub.confirmed_result();
    published.server_ticks = hub.server_tick_distribution();
    published.error = None;
    published.phase = if hub.confirmed_result().is_some() {
        WebRoomWorkerPhase::Finished
    } else if hub.authority().simulation().current_tick() != SimTick::ZERO {
        WebRoomWorkerPhase::Fighting
    } else if hub.countdown_start_tick().is_some() {
        WebRoomWorkerPhase::Countdown
    } else {
        WebRoomWorkerPhase::WaitingForPeers
    };
}

fn authority_deadline(epoch: Instant, network_tick: u64) -> Result<Instant, WebRoomWorkerError> {
    let nanos = u128::from(network_tick)
        .checked_mul(NANOS_PER_SECOND)
        .ok_or(WebRoomWorkerError::TimelineExhausted)?
        / AUTHORITY_HZ;
    let nanos = u64::try_from(nanos).map_err(|_| WebRoomWorkerError::TimelineExhausted)?;
    epoch
        .checked_add(Duration::from_nanos(nanos))
        .ok_or(WebRoomWorkerError::TimelineExhausted)
}

fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rational_deadline_has_exact_sixty_tick_second_boundaries() {
        let epoch = Instant::now();
        assert_eq!(
            authority_deadline(epoch, 1).unwrap() - epoch,
            Duration::from_nanos(16_666_666)
        );
        assert_eq!(
            authority_deadline(epoch, 60).unwrap() - epoch,
            Duration::from_secs(1)
        );
        assert_eq!(
            authority_deadline(epoch, 600).unwrap() - epoch,
            Duration::from_secs(10)
        );
    }

    #[test]
    fn worker_configuration_is_strictly_bounded() {
        assert!(WebRoomWorkerConfig::default().validate().is_ok());
        assert_eq!(
            WebRoomWorkerConfig {
                command_capacity: 0,
                ..WebRoomWorkerConfig::default()
            }
            .validate(),
            Err(WebRoomWorkerError::InvalidConfiguration)
        );
    }
}
