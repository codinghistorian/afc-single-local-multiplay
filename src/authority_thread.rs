//! Bounded real-time and unpaced runners for [`AuthorityMatch`](crate::authority::AuthorityMatch).
//!
//! Wall time is used only to decide when the worker calls `AuthorityMatch::step`.
//! No elapsed time enters the deterministic simulation. Input submission is
//! nonblocking, tick observation is latest-wins, and terminal/result delivery is
//! kept separate from the lossy render-facing report path.

use crate::authority::{
    AuthorityMatch, AuthorityMatchError, AuthoritySimulation, AuthorityTickReport,
};
use crate::authority_input::{FrameIngestOutcome, InputIngestReport};
use crate::network_protocol::{InputBatch, InputFrame, PeerId, SimTick, StateHash};
use std::fmt;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub const AUTHORITY_THREAD_TICK_RATE_HZ: u32 = 60;
pub const DEFAULT_AUTHORITY_COMMAND_CAPACITY: usize = 256;
pub const MAX_AUTHORITY_COMMAND_CAPACITY: usize = 4_096;
pub const DEFAULT_MAX_COMMANDS_PER_SERVICE: usize = 64;
pub const MAX_COMMANDS_PER_SERVICE: usize = 4_096;

const REPORT_SIGNAL_CAPACITY: usize = 1;
const RESULT_SIGNAL_CAPACITY: usize = 1;
const TERMINAL_SIGNAL_CAPACITY: usize = 1;
const NANOS_PER_SECOND: u64 = 1_000_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthorityThreadConfig {
    pub command_capacity: usize,
    /// Limits input work before each overdue tick so an input flood cannot
    /// indefinitely starve simulation or shutdown.
    pub max_commands_per_service: usize,
}

impl Default for AuthorityThreadConfig {
    fn default() -> Self {
        Self {
            command_capacity: DEFAULT_AUTHORITY_COMMAND_CAPACITY,
            max_commands_per_service: DEFAULT_MAX_COMMANDS_PER_SERVICE,
        }
    }
}

impl AuthorityThreadConfig {
    pub fn validate(self) -> Result<(), AuthorityThreadConfigError> {
        if self.command_capacity == 0 || self.command_capacity > MAX_AUTHORITY_COMMAND_CAPACITY {
            return Err(AuthorityThreadConfigError::CommandCapacity {
                value: self.command_capacity,
                max: MAX_AUTHORITY_COMMAND_CAPACITY,
            });
        }
        if self.max_commands_per_service == 0
            || self.max_commands_per_service > MAX_COMMANDS_PER_SERVICE
        {
            return Err(AuthorityThreadConfigError::CommandServiceLimit {
                value: self.max_commands_per_service,
                max: MAX_COMMANDS_PER_SERVICE,
            });
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthorityThreadConfigError {
    CommandCapacity { value: usize, max: usize },
    CommandServiceLimit { value: usize, max: usize },
}

impl fmt::Display for AuthorityThreadConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid authority thread configuration: {self:?}"
        )
    }
}

impl std::error::Error for AuthorityThreadConfigError {}

#[derive(Debug)]
pub enum AuthorityThreadSpawnError {
    InvalidConfig(AuthorityThreadConfigError),
    TickRateMismatch { manifest_hz: u16, required_hz: u32 },
    Spawn(io::Error),
}

impl fmt::Display for AuthorityThreadSpawnError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(error) => error.fmt(formatter),
            Self::TickRateMismatch {
                manifest_hz,
                required_hz,
            } => write!(
                formatter,
                "authority manifest uses {manifest_hz} Hz; thread runner requires {required_hz} Hz"
            ),
            Self::Spawn(error) => write!(formatter, "failed to spawn authority thread: {error}"),
        }
    }
}

impl std::error::Error for AuthorityThreadSpawnError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthorityThreadCommand {
    PeerInputBatch { peer: PeerId, batch: InputBatch },
    BotInputFrame(InputFrame),
    Stop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthorityCommandSubmitOutcome {
    Queued,
    Full,
    Disconnected,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AuthorityThreadMetrics {
    pub command_queue_depth: usize,
    pub command_queue_high_water: usize,
    pub commands_queued: u64,
    pub commands_full: u64,
    pub commands_disconnected: u64,
    pub commands_processed: u64,
    pub peer_batches_processed: u64,
    pub bot_frames_processed: u64,
    pub stop_commands_processed: u64,
    pub peer_batch_protocol_errors: u64,
    pub rejected_bot_frames: u64,
    pub late_input_frames: u64,
    pub simulated_ticks: u64,
    pub late_tick_starts: u64,
    pub maximum_tick_lateness_ns: u64,
    pub total_step_duration_ns: u64,
    pub maximum_step_duration_ns: u64,
    pub over_budget_steps: u64,
    pub tick_reports_published: u64,
    pub tick_reports_coalesced: u64,
    pub report_notification_high_water: usize,
}

#[derive(Default)]
struct SharedMetrics {
    command_queue_depth: AtomicUsize,
    command_queue_high_water: AtomicUsize,
    commands_queued: AtomicU64,
    commands_full: AtomicU64,
    commands_disconnected: AtomicU64,
    commands_processed: AtomicU64,
    peer_batches_processed: AtomicU64,
    bot_frames_processed: AtomicU64,
    stop_commands_processed: AtomicU64,
    peer_batch_protocol_errors: AtomicU64,
    rejected_bot_frames: AtomicU64,
    late_input_frames: AtomicU64,
    simulated_ticks: AtomicU64,
    late_tick_starts: AtomicU64,
    maximum_tick_lateness_ns: AtomicU64,
    total_step_duration_ns: AtomicU64,
    maximum_step_duration_ns: AtomicU64,
    over_budget_steps: AtomicU64,
    tick_reports_published: AtomicU64,
    tick_reports_coalesced: AtomicU64,
    report_notification_high_water: AtomicUsize,
}

impl SharedMetrics {
    fn snapshot(&self, capacity: usize) -> AuthorityThreadMetrics {
        AuthorityThreadMetrics {
            command_queue_depth: self
                .command_queue_depth
                .load(Ordering::Relaxed)
                .min(capacity),
            command_queue_high_water: self
                .command_queue_high_water
                .load(Ordering::Relaxed)
                .min(capacity),
            commands_queued: self.commands_queued.load(Ordering::Relaxed),
            commands_full: self.commands_full.load(Ordering::Relaxed),
            commands_disconnected: self.commands_disconnected.load(Ordering::Relaxed),
            commands_processed: self.commands_processed.load(Ordering::Relaxed),
            peer_batches_processed: self.peer_batches_processed.load(Ordering::Relaxed),
            bot_frames_processed: self.bot_frames_processed.load(Ordering::Relaxed),
            stop_commands_processed: self.stop_commands_processed.load(Ordering::Relaxed),
            peer_batch_protocol_errors: self.peer_batch_protocol_errors.load(Ordering::Relaxed),
            rejected_bot_frames: self.rejected_bot_frames.load(Ordering::Relaxed),
            late_input_frames: self.late_input_frames.load(Ordering::Relaxed),
            simulated_ticks: self.simulated_ticks.load(Ordering::Relaxed),
            late_tick_starts: self.late_tick_starts.load(Ordering::Relaxed),
            maximum_tick_lateness_ns: self.maximum_tick_lateness_ns.load(Ordering::Relaxed),
            total_step_duration_ns: self.total_step_duration_ns.load(Ordering::Relaxed),
            maximum_step_duration_ns: self.maximum_step_duration_ns.load(Ordering::Relaxed),
            over_budget_steps: self.over_budget_steps.load(Ordering::Relaxed),
            tick_reports_published: self.tick_reports_published.load(Ordering::Relaxed),
            tick_reports_coalesced: self.tick_reports_coalesced.load(Ordering::Relaxed),
            report_notification_high_water: self
                .report_notification_high_water
                .load(Ordering::Relaxed)
                .min(REPORT_SIGNAL_CAPACITY),
        }
    }
}

fn update_max_u64(target: &AtomicU64, value: u64) {
    let _ = target.fetch_max(value, Ordering::Relaxed);
}

fn update_max_usize(target: &AtomicUsize, value: usize) {
    let _ = target.fetch_max(value, Ordering::Relaxed);
}

fn nanos_u64(duration: Duration) -> u64 {
    duration.as_nanos().min(u128::from(u64::MAX)) as u64
}

#[derive(Clone)]
pub struct AuthorityCommandSender {
    sender: SyncSender<AuthorityThreadCommand>,
    metrics: Arc<SharedMetrics>,
    capacity: usize,
}

impl AuthorityCommandSender {
    pub fn try_submit_peer_batch(
        &self,
        peer: PeerId,
        batch: InputBatch,
    ) -> AuthorityCommandSubmitOutcome {
        self.try_submit(AuthorityThreadCommand::PeerInputBatch { peer, batch })
    }

    pub fn try_submit_bot_frame(&self, frame: InputFrame) -> AuthorityCommandSubmitOutcome {
        self.try_submit(AuthorityThreadCommand::BotInputFrame(frame))
    }

    pub fn try_stop(&self) -> AuthorityCommandSubmitOutcome {
        self.try_submit(AuthorityThreadCommand::Stop)
    }

    pub fn metrics(&self) -> AuthorityThreadMetrics {
        self.metrics.snapshot(self.capacity)
    }

    fn try_submit(&self, command: AuthorityThreadCommand) -> AuthorityCommandSubmitOutcome {
        // Reserve before publishing so the consumer can never underflow. Failed
        // reservations are rolled back; the observable depth is clamped to the
        // physical sync_channel bound.
        let reserved = self
            .metrics
            .command_queue_depth
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1);
        match self.sender.try_send(command) {
            Ok(()) => {
                self.metrics.commands_queued.fetch_add(1, Ordering::Relaxed);
                update_max_usize(
                    &self.metrics.command_queue_high_water,
                    reserved.min(self.capacity),
                );
                AuthorityCommandSubmitOutcome::Queued
            }
            Err(TrySendError::Full(_)) => {
                self.metrics
                    .command_queue_depth
                    .fetch_sub(1, Ordering::AcqRel);
                self.metrics.commands_full.fetch_add(1, Ordering::Relaxed);
                AuthorityCommandSubmitOutcome::Full
            }
            Err(TrySendError::Disconnected(_)) => {
                self.metrics
                    .command_queue_depth
                    .fetch_sub(1, Ordering::AcqRel);
                self.metrics
                    .commands_disconnected
                    .fetch_add(1, Ordering::Relaxed);
                AuthorityCommandSubmitOutcome::Disconnected
            }
        }
    }
}

#[derive(Default)]
struct LatestReportState {
    generation: u64,
    consumed_generation: u64,
    report: Option<AuthorityTickReport>,
}

struct TickReportPublisher {
    state: Arc<Mutex<LatestReportState>>,
    signal: SyncSender<()>,
    metrics: Arc<SharedMetrics>,
}

impl TickReportPublisher {
    fn publish(&self, report: AuthorityTickReport) {
        {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            if state.report.is_some() && state.generation != state.consumed_generation {
                self.metrics
                    .tick_reports_coalesced
                    .fetch_add(1, Ordering::Relaxed);
            }
            state.generation = state.generation.wrapping_add(1).max(1);
            state.report = Some(report);
        }
        self.metrics
            .tick_reports_published
            .fetch_add(1, Ordering::Relaxed);
        match self.signal.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) => update_max_usize(
                &self.metrics.report_notification_high_water,
                REPORT_SIGNAL_CAPACITY,
            ),
            Err(TrySendError::Disconnected(())) => {}
        }
    }
}

/// Single-consumer, latest-wins tick observation. It retains exactly one report.
/// Consumers that stall skip intermediate reports without ever blocking authority.
pub struct LatestAuthorityTickReports {
    state: Arc<Mutex<LatestReportState>>,
    signal: Receiver<()>,
    last_seen_generation: u64,
}

impl LatestAuthorityTickReports {
    pub fn latest(&self) -> Option<AuthorityTickReport> {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .report
    }

    pub fn try_recv_latest(&mut self) -> Result<AuthorityTickReport, TryRecvError> {
        if let Some(report) = self.take_unseen() {
            return Ok(report);
        }
        loop {
            match self.signal.try_recv() {
                Ok(()) => {
                    if let Some(report) = self.take_unseen() {
                        return Ok(report);
                    }
                }
                Err(error) => return self.take_unseen().ok_or(error),
            }
        }
    }

    pub fn recv_latest_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<AuthorityTickReport, RecvTimeoutError> {
        if let Some(report) = self.take_unseen() {
            return Ok(report);
        }
        let deadline = Instant::now().checked_add(timeout);
        loop {
            let remaining = deadline
                .map(|deadline| deadline.saturating_duration_since(Instant::now()))
                .unwrap_or(timeout);
            match self.signal.recv_timeout(remaining) {
                Ok(()) => {
                    if let Some(report) = self.take_unseen() {
                        return Ok(report);
                    }
                }
                Err(error) => return self.take_unseen().ok_or(error),
            }
        }
    }

    fn take_unseen(&mut self) -> Option<AuthorityTickReport> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.generation == 0 || state.generation == self.last_seen_generation {
            return None;
        }
        self.last_seen_generation = state.generation;
        state.consumed_generation = state.generation;
        state.report
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthorityFinalResultReport {
    pub tick: SimTick,
    pub state_hash: StateHash,
    pub result_id: u64,
}

#[derive(Debug)]
pub enum AuthorityThreadExit<E> {
    StopRequested,
    CommandChannelDisconnected,
    MatchFinished {
        result_id: u64,
    },
    /// Construction was intentionally deferred onto the worker because the
    /// simulation world is thread-affine (for example, Bevy `App` is `!Send`).
    BootstrapError(String),
    AuthorityError(AuthorityMatchError<E>),
}

#[derive(Debug)]
pub struct AuthorityThreadTerminal<E> {
    pub exit: AuthorityThreadExit<E>,
    pub last_tick: SimTick,
    pub final_result: Option<AuthorityFinalResultReport>,
    pub metrics: AuthorityThreadMetrics,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthorityThreadJoinError {
    WorkerPanicked,
    TerminalAlreadyTaken,
}

impl fmt::Display for AuthorityThreadJoinError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "authority thread join failed: {self:?}")
    }
}

impl std::error::Error for AuthorityThreadJoinError {}

pub struct AuthorityThreadHandle<E: Send + 'static> {
    commands: AuthorityCommandSender,
    reports: LatestAuthorityTickReports,
    result: Arc<Mutex<Option<AuthorityFinalResultReport>>>,
    result_signal: Receiver<()>,
    terminal: Arc<Mutex<Option<AuthorityThreadTerminal<E>>>>,
    terminal_signal: Receiver<()>,
    force_shutdown: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl<E: Send + 'static> AuthorityThreadHandle<E> {
    pub fn command_sender(&self) -> AuthorityCommandSender {
        self.commands.clone()
    }

    pub fn try_submit_peer_batch(
        &self,
        peer: PeerId,
        batch: InputBatch,
    ) -> AuthorityCommandSubmitOutcome {
        self.commands.try_submit_peer_batch(peer, batch)
    }

    pub fn try_submit_bot_frame(&self, frame: InputFrame) -> AuthorityCommandSubmitOutcome {
        self.commands.try_submit_bot_frame(frame)
    }

    pub fn try_stop(&self) -> AuthorityCommandSubmitOutcome {
        self.commands.try_stop()
    }

    pub fn reports_mut(&mut self) -> &mut LatestAuthorityTickReports {
        &mut self.reports
    }

    pub fn final_result(&self) -> Option<AuthorityFinalResultReport> {
        *self
            .result
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    pub fn wait_for_result(
        &self,
        timeout: Duration,
    ) -> Result<AuthorityFinalResultReport, RecvTimeoutError> {
        if let Some(result) = self.final_result() {
            return Ok(result);
        }
        match self.result_signal.recv_timeout(timeout) {
            Ok(()) => self.final_result().ok_or(RecvTimeoutError::Disconnected),
            Err(error) => self.final_result().ok_or(error),
        }
    }

    pub fn metrics(&self) -> AuthorityThreadMetrics {
        self.commands.metrics()
    }

    pub fn wait_for_terminal(
        &self,
        timeout: Duration,
    ) -> Result<AuthorityThreadTerminal<E>, RecvTimeoutError> {
        if let Some(terminal) = self.take_terminal() {
            return Ok(terminal);
        }
        match self.terminal_signal.recv_timeout(timeout) {
            Ok(()) => self.take_terminal().ok_or(RecvTimeoutError::Disconnected),
            Err(error) => self.take_terminal().ok_or(error),
        }
    }

    pub fn is_finished(&self) -> bool {
        self.join.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// Guaranteed cooperative shutdown path. Unlike FIFO `try_stop`, this cannot
    /// be prevented by a full command queue.
    pub fn request_shutdown(&self) {
        self.force_shutdown.store(true, Ordering::Release);
        let _ = self.commands.try_stop();
    }

    pub fn shutdown(mut self) -> Result<AuthorityThreadTerminal<E>, AuthorityThreadJoinError> {
        self.request_shutdown();
        self.join_worker()?;
        self.take_terminal()
            .ok_or(AuthorityThreadJoinError::TerminalAlreadyTaken)
    }

    /// Join after observing/taking terminal state through `wait_for_terminal`.
    pub fn join(mut self) -> Result<(), AuthorityThreadJoinError> {
        self.join_worker()
    }

    fn take_terminal(&self) -> Option<AuthorityThreadTerminal<E>> {
        self.terminal
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
    }

    fn join_worker(&mut self) -> Result<(), AuthorityThreadJoinError> {
        let Some(join) = self.join.take() else {
            return Ok(());
        };
        join.join()
            .map_err(|_| AuthorityThreadJoinError::WorkerPanicked)
    }
}

impl<E: Send + 'static> Drop for AuthorityThreadHandle<E> {
    fn drop(&mut self) {
        self.force_shutdown.store(true, Ordering::Release);
        let _ = self.commands.try_stop();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// Starts a dedicated, absolutely scheduled 60 Hz authority worker.
pub fn spawn_authority_thread<S>(
    authority: AuthorityMatch<S>,
    config: AuthorityThreadConfig,
) -> Result<AuthorityThreadHandle<S::Error>, AuthorityThreadSpawnError>
where
    S: AuthoritySimulation + Send + 'static,
    S::Snapshot: Send + 'static,
    S::Error: Send + 'static,
{
    let manifest_hz = authority.manifest().tick_rate_hz;
    spawn_authority_thread_from_factory_inner(manifest_hz, config, move || {
        Ok::<_, std::convert::Infallible>(authority)
    })
}

/// Starts a 60 Hz worker which constructs its simulation on the worker thread.
///
/// Some production worlds are deliberately thread-affine and therefore cannot
/// be built on one thread and moved to another. The factory itself must be
/// `Send`, but the simulation it creates does not: it is born, stepped, and
/// dropped on the authority worker. Bootstrap failure is published through the
/// reliable terminal channel as [`AuthorityThreadExit::BootstrapError`].
pub fn spawn_authority_thread_from_factory<S, F, B>(
    manifest_hz: u16,
    config: AuthorityThreadConfig,
    factory: F,
) -> Result<AuthorityThreadHandle<S::Error>, AuthorityThreadSpawnError>
where
    S: AuthoritySimulation + 'static,
    S::Error: Send + 'static,
    F: FnOnce() -> Result<AuthorityMatch<S>, B> + Send + 'static,
    B: fmt::Display + 'static,
{
    spawn_authority_thread_from_factory_inner(manifest_hz, config, factory)
}

fn spawn_authority_thread_from_factory_inner<S, F, B>(
    manifest_hz: u16,
    config: AuthorityThreadConfig,
    factory: F,
) -> Result<AuthorityThreadHandle<S::Error>, AuthorityThreadSpawnError>
where
    S: AuthoritySimulation + 'static,
    S::Error: Send + 'static,
    F: FnOnce() -> Result<AuthorityMatch<S>, B> + Send + 'static,
    B: fmt::Display + 'static,
{
    config
        .validate()
        .map_err(AuthorityThreadSpawnError::InvalidConfig)?;
    if u32::from(manifest_hz) != AUTHORITY_THREAD_TICK_RATE_HZ {
        return Err(AuthorityThreadSpawnError::TickRateMismatch {
            manifest_hz,
            required_hz: AUTHORITY_THREAD_TICK_RATE_HZ,
        });
    }

    let metrics = Arc::new(SharedMetrics::default());
    let (command_tx, command_rx) = mpsc::sync_channel(config.command_capacity);
    let commands = AuthorityCommandSender {
        sender: command_tx,
        metrics: Arc::clone(&metrics),
        capacity: config.command_capacity,
    };
    let report_state = Arc::new(Mutex::new(LatestReportState::default()));
    let (report_signal_tx, report_signal_rx) = mpsc::sync_channel(REPORT_SIGNAL_CAPACITY);
    let report_publisher = TickReportPublisher {
        state: Arc::clone(&report_state),
        signal: report_signal_tx,
        metrics: Arc::clone(&metrics),
    };
    let result = Arc::new(Mutex::new(None));
    let result_worker = Arc::clone(&result);
    let (result_signal_tx, result_signal_rx) = mpsc::sync_channel(RESULT_SIGNAL_CAPACITY);
    let terminal = Arc::new(Mutex::new(None));
    let terminal_worker = Arc::clone(&terminal);
    let (terminal_signal_tx, terminal_signal_rx) = mpsc::sync_channel(TERMINAL_SIGNAL_CAPACITY);
    let force_shutdown = Arc::new(AtomicBool::new(false));
    let force_shutdown_worker = Arc::clone(&force_shutdown);
    let worker_metrics = Arc::clone(&metrics);
    let capacity = config.command_capacity;

    let join = thread::Builder::new()
        .name("afc-authority-60hz".to_owned())
        .spawn(move || {
            let terminal_value = match factory() {
                Ok(authority) => run_realtime_worker(
                    authority,
                    config,
                    command_rx,
                    report_publisher,
                    &result_worker,
                    &result_signal_tx,
                    &force_shutdown_worker,
                    &worker_metrics,
                    capacity,
                ),
                Err(error) => AuthorityThreadTerminal {
                    exit: AuthorityThreadExit::BootstrapError(error.to_string()),
                    last_tick: SimTick::ZERO,
                    final_result: None,
                    metrics: worker_metrics.snapshot(capacity),
                },
            };
            *terminal_worker
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(terminal_value);
            // Exactly one terminal publication exists, so capacity one is
            // reliable without blocking the worker.
            let _ = terminal_signal_tx.try_send(());
        })
        .map_err(AuthorityThreadSpawnError::Spawn)?;

    Ok(AuthorityThreadHandle {
        commands,
        reports: LatestAuthorityTickReports {
            state: report_state,
            signal: report_signal_rx,
            last_seen_generation: 0,
        },
        result,
        result_signal: result_signal_rx,
        terminal,
        terminal_signal: terminal_signal_rx,
        force_shutdown,
        join: Some(join),
    })
}

enum CommandService {
    Continue,
    Stop,
}

fn dequeue(metrics: &SharedMetrics) {
    metrics.command_queue_depth.fetch_sub(1, Ordering::AcqRel);
    metrics.commands_processed.fetch_add(1, Ordering::Relaxed);
}

fn service_command<S: AuthoritySimulation>(
    authority: &mut AuthorityMatch<S>,
    command: AuthorityThreadCommand,
    metrics: &SharedMetrics,
) -> CommandService {
    match command {
        AuthorityThreadCommand::PeerInputBatch { peer, batch } => {
            metrics
                .peer_batches_processed
                .fetch_add(1, Ordering::Relaxed);
            let current = authority.simulation().current_tick();
            let late = batch
                .as_slice()
                .iter()
                .flat_map(|window| window.as_slice())
                .filter(|frame| frame.tick.0 <= current.0)
                .count() as u64;
            metrics.late_input_frames.fetch_add(late, Ordering::Relaxed);
            if authority.ingest_peer_batch(peer, &batch).is_err() {
                metrics
                    .peer_batch_protocol_errors
                    .fetch_add(1, Ordering::Relaxed);
            }
            CommandService::Continue
        }
        AuthorityThreadCommand::BotInputFrame(frame) => {
            metrics.bot_frames_processed.fetch_add(1, Ordering::Relaxed);
            if frame.tick.0 <= authority.simulation().current_tick().0 {
                metrics.late_input_frames.fetch_add(1, Ordering::Relaxed);
            }
            if matches!(
                authority.ingest_bot_frame(frame),
                FrameIngestOutcome::Rejected(_)
            ) {
                metrics.rejected_bot_frames.fetch_add(1, Ordering::Relaxed);
            }
            CommandService::Continue
        }
        AuthorityThreadCommand::Stop => {
            metrics
                .stop_commands_processed
                .fetch_add(1, Ordering::Relaxed);
            CommandService::Stop
        }
    }
}

/// Absolute-deadline cadence shared by every in-process authority worker.
///
/// Keeping this scheduler independent from Bevy's fixed clock ensures listen
/// authority deadlines continue while the render thread is blocked. The
/// ordinal calculation avoids accumulating duration-rounding drift.
pub(crate) struct SixtyHzSchedule {
    epoch: Instant,
    ordinal: u64,
}

impl SixtyHzSchedule {
    pub(crate) fn new() -> Self {
        Self {
            epoch: Instant::now(),
            ordinal: 1,
        }
    }

    pub(crate) fn deadline(&self) -> Instant {
        let seconds = self.ordinal / u64::from(AUTHORITY_THREAD_TICK_RATE_HZ);
        let remainder = self.ordinal % u64::from(AUTHORITY_THREAD_TICK_RATE_HZ);
        let nanos = remainder * NANOS_PER_SECOND / u64::from(AUTHORITY_THREAD_TICK_RATE_HZ);
        self.epoch
            .checked_add(Duration::new(seconds, nanos as u32))
            .unwrap_or_else(Instant::now)
    }

    pub(crate) fn advance(&mut self) {
        self.ordinal = self.ordinal.saturating_add(1);
    }
}

#[allow(clippy::too_many_arguments)]
fn run_realtime_worker<S>(
    mut authority: AuthorityMatch<S>,
    config: AuthorityThreadConfig,
    command_rx: Receiver<AuthorityThreadCommand>,
    reports: TickReportPublisher,
    result_slot: &Arc<Mutex<Option<AuthorityFinalResultReport>>>,
    result_signal: &SyncSender<()>,
    force_shutdown: &AtomicBool,
    metrics: &SharedMetrics,
    capacity: usize,
) -> AuthorityThreadTerminal<S::Error>
where
    S: AuthoritySimulation,
{
    let mut schedule = SixtyHzSchedule::new();
    let mut last_tick = authority.simulation().current_tick();
    let mut final_result = None;

    let exit = 'worker: loop {
        if force_shutdown.load(Ordering::Acquire) {
            break AuthorityThreadExit::StopRequested;
        }

        let deadline = schedule.deadline();
        let now = Instant::now();
        if now < deadline {
            match command_rx.recv_timeout(deadline.duration_since(now)) {
                Ok(command) => {
                    dequeue(metrics);
                    if matches!(
                        service_command(&mut authority, command, metrics),
                        CommandService::Stop
                    ) {
                        break AuthorityThreadExit::StopRequested;
                    }
                    continue;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    break AuthorityThreadExit::CommandChannelDisconnected;
                }
            }
        }

        for _ in 0..config.max_commands_per_service {
            if force_shutdown.load(Ordering::Acquire) {
                break 'worker AuthorityThreadExit::StopRequested;
            }
            match command_rx.try_recv() {
                Ok(command) => {
                    dequeue(metrics);
                    if matches!(
                        service_command(&mut authority, command, metrics),
                        CommandService::Stop
                    ) {
                        break 'worker AuthorityThreadExit::StopRequested;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    break 'worker AuthorityThreadExit::CommandChannelDisconnected;
                }
            }
        }
        if force_shutdown.load(Ordering::Acquire) {
            break AuthorityThreadExit::StopRequested;
        }

        let lateness = Instant::now().saturating_duration_since(deadline);
        if !lateness.is_zero() {
            metrics.late_tick_starts.fetch_add(1, Ordering::Relaxed);
            update_max_u64(&metrics.maximum_tick_lateness_ns, nanos_u64(lateness));
        }
        let step_start = Instant::now();
        let step = authority.step();
        let step_duration = step_start.elapsed();
        let step_ns = nanos_u64(step_duration);
        metrics
            .total_step_duration_ns
            .fetch_add(step_ns, Ordering::Relaxed);
        update_max_u64(&metrics.maximum_step_duration_ns, step_ns);
        if step_duration > Duration::from_nanos(NANOS_PER_SECOND / 60) {
            metrics.over_budget_steps.fetch_add(1, Ordering::Relaxed);
        }

        let report = match step {
            Ok(report) => report,
            Err(error) => break AuthorityThreadExit::AuthorityError(error),
        };
        last_tick = report.tick;
        metrics.simulated_ticks.fetch_add(1, Ordering::Relaxed);
        reports.publish(report);
        schedule.advance();

        if let Some(result_id) = report.final_result_id {
            let result = AuthorityFinalResultReport {
                tick: report.tick,
                state_hash: report.state_hash,
                result_id,
            };
            *result_slot
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(result);
            // At most one result is published because the worker exits now.
            let _ = result_signal.try_send(());
            final_result = Some(result);
            break AuthorityThreadExit::MatchFinished { result_id };
        }
    };

    // The worker no longer consumes ingress. Publish a stable external queue
    // gauge even if shutdown intentionally abandons queued observational work.
    metrics.command_queue_depth.store(0, Ordering::Relaxed);
    AuthorityThreadTerminal {
        exit,
        last_tick,
        final_result,
        metrics: metrics.snapshot(capacity),
    }
}

/// Deterministic unpaced runner for tests, replay verification, and headless jobs.
pub struct ManualAuthorityRunner<S: AuthoritySimulation> {
    authority: AuthorityMatch<S>,
    simulated_ticks: u64,
    total_step_duration_ns: u64,
    maximum_step_duration_ns: u64,
}

impl<S: AuthoritySimulation> ManualAuthorityRunner<S> {
    pub fn new(authority: AuthorityMatch<S>) -> Self {
        Self {
            authority,
            simulated_ticks: 0,
            total_step_duration_ns: 0,
            maximum_step_duration_ns: 0,
        }
    }

    pub fn authority(&self) -> &AuthorityMatch<S> {
        &self.authority
    }

    pub fn authority_mut(&mut self) -> &mut AuthorityMatch<S> {
        &mut self.authority
    }

    pub fn ingest_peer_batch(
        &mut self,
        peer: PeerId,
        batch: &InputBatch,
    ) -> Result<InputIngestReport, crate::network_protocol::ProtocolValidationError> {
        self.authority.ingest_peer_batch(peer, batch)
    }

    pub fn ingest_bot_frame(&mut self, frame: InputFrame) -> FrameIngestOutcome {
        self.authority.ingest_bot_frame(frame)
    }

    pub fn step(&mut self) -> Result<AuthorityTickReport, AuthorityMatchError<S::Error>> {
        let start = Instant::now();
        let result = self.authority.step();
        let elapsed = nanos_u64(start.elapsed());
        self.total_step_duration_ns = self.total_step_duration_ns.saturating_add(elapsed);
        self.maximum_step_duration_ns = self.maximum_step_duration_ns.max(elapsed);
        if result.is_ok() {
            self.simulated_ticks = self.simulated_ticks.saturating_add(1);
        }
        result
    }

    pub fn run_ticks(
        &mut self,
        count: u32,
        mut observe: impl FnMut(&AuthorityTickReport),
    ) -> Result<(), AuthorityMatchError<S::Error>> {
        for _ in 0..count {
            let report = self.step()?;
            observe(&report);
            if report.final_result_id.is_some() {
                break;
            }
        }
        Ok(())
    }

    pub fn simulated_ticks(&self) -> u64 {
        self.simulated_ticks
    }

    pub fn total_step_duration_ns(&self) -> u64 {
        self.total_step_duration_ns
    }

    pub fn maximum_step_duration_ns(&self) -> u64 {
        self.maximum_step_duration_ns
    }

    pub fn into_inner(self) -> AuthorityMatch<S> {
        self.authority
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority::{AuthoritySimulation, AuthoritySnapshot};
    use crate::authority_input::{AuthorityInputConfig, CommittedTickInputs};
    use crate::network_protocol::{
        AuthorityKind, BuildId, CompatibilityId, DefinitionId, FighterId, FighterSlotConfig,
        GameplayContentHash, ManifestHash, MatchId, MatchManifest, ProtocolValidationError,
        ProtocolVersion, ReplayFormatVersion, SeatAssignment, SeatId, SeatOwner, SeatOwnership,
        SimulationVersion, TeamId,
    };

    #[derive(Clone)]
    struct ToySnapshot {
        tick: SimTick,
        hash: u64,
    }

    impl AuthoritySnapshot for ToySnapshot {
        fn tick(&self) -> SimTick {
            self.tick
        }

        fn state_hash(&self) -> Result<StateHash, ProtocolValidationError> {
            Ok(StateHash(self.hash))
        }
    }

    struct ToySimulation {
        tick: SimTick,
        hash: u64,
        finish_tick: Option<SimTick>,
        ticks: Arc<Mutex<Vec<SimTick>>>,
    }

    impl AuthoritySimulation for ToySimulation {
        type Snapshot = ToySnapshot;
        type Error = &'static str;

        fn current_tick(&self) -> SimTick {
            self.tick
        }

        fn step(&mut self, inputs: &CommittedTickInputs) -> Result<(), Self::Error> {
            if inputs.tick != self.tick.next() {
                return Err("non-contiguous tick");
            }
            self.tick = inputs.tick;
            self.hash = self.hash.wrapping_mul(31).wrapping_add(self.tick.0);
            self.ticks
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(self.tick);
            Ok(())
        }

        fn capture_snapshot(&self) -> Result<Self::Snapshot, Self::Error> {
            Ok(ToySnapshot {
                tick: self.tick,
                hash: self.hash,
            })
        }

        fn final_result_id(&self) -> Option<u64> {
            (Some(self.tick) == self.finish_tick).then_some(77)
        }
    }

    fn peer() -> PeerId {
        PeerId::new(7).unwrap()
    }

    fn match_id() -> MatchId {
        MatchId::new([3; 16]).unwrap()
    }

    fn manifest() -> MatchManifest {
        let ownership = SeatOwnership::from_assignments(&[SeatAssignment {
            seat: SeatId::new(0).unwrap(),
            fighter: FighterId::new(0).unwrap(),
            owner: SeatOwner::Peer(peer()),
        }])
        .unwrap();
        let mut slots = [FighterSlotConfig::default(); 4];
        slots[0] = FighterSlotConfig {
            occupied: true,
            fighter: FighterId::new(0).unwrap(),
            team: TeamId::new(0).unwrap(),
            character: DefinitionId::new(1).unwrap(),
            style: DefinitionId::new(1).unwrap(),
            equipment: DefinitionId::new(0).unwrap(),
        };
        MatchManifest {
            compatibility: CompatibilityId {
                protocol: ProtocolVersion::new(1).unwrap(),
                simulation: SimulationVersion::new(1).unwrap(),
                replay: ReplayFormatVersion::new(1).unwrap(),
                build: BuildId::new([1; 16]).unwrap(),
                gameplay_content: GameplayContentHash::new([2; 32]).unwrap(),
            },
            manifest_hash: ManifestHash(9),
            match_id: match_id(),
            authority: AuthorityKind::Listen,
            trusted_results: false,
            arena: DefinitionId::new(1).unwrap(),
            rules: DefinitionId::new(1).unwrap(),
            slots,
            ownership,
            master_gameplay_seed: 99,
            rng_scheme_version: 1,
            tick_rate_hz: 60,
            input_delay_ticks: 2,
            rollback_limit_ticks: 12,
            snapshot_history_ticks: 32,
            agreed_start_tick: SimTick(30),
        }
    }

    fn authority(
        finish_tick: Option<u64>,
        ticks: Arc<Mutex<Vec<SimTick>>>,
    ) -> AuthorityMatch<ToySimulation> {
        AuthorityMatch::new(
            manifest(),
            ToySimulation {
                tick: SimTick::ZERO,
                hash: 17,
                finish_tick: finish_tick.map(SimTick),
                ticks,
            },
            AuthorityInputConfig::default(),
        )
        .unwrap()
    }

    #[test]
    fn config_rejects_unbounded_or_rendezvous_queues() {
        assert!(
            AuthorityThreadConfig {
                command_capacity: 0,
                ..AuthorityThreadConfig::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            AuthorityThreadConfig {
                command_capacity: MAX_AUTHORITY_COMMAND_CAPACITY + 1,
                ..AuthorityThreadConfig::default()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn command_queue_is_bounded_and_full_is_observable() {
        let metrics = Arc::new(SharedMetrics::default());
        let (tx, rx) = mpsc::sync_channel(2);
        let sender = AuthorityCommandSender {
            sender: tx,
            metrics,
            capacity: 2,
        };
        assert_eq!(sender.try_stop(), AuthorityCommandSubmitOutcome::Queued);
        assert_eq!(sender.try_stop(), AuthorityCommandSubmitOutcome::Queued);
        assert_eq!(sender.try_stop(), AuthorityCommandSubmitOutcome::Full);
        assert_eq!(sender.metrics().command_queue_high_water, 2);
        drop(rx);
        assert_eq!(
            sender.try_stop(),
            AuthorityCommandSubmitOutcome::Disconnected
        );
    }

    #[test]
    fn deferred_bootstrap_failure_is_published_reliably() {
        let handle = spawn_authority_thread_from_factory::<ToySimulation, _, _>(
            AUTHORITY_THREAD_TICK_RATE_HZ as u16,
            AuthorityThreadConfig::default(),
            || Err::<AuthorityMatch<ToySimulation>, _>("fixture bootstrap rejected"),
        )
        .unwrap();

        let terminal = handle.wait_for_terminal(Duration::from_secs(1)).unwrap();
        assert!(matches!(
            terminal.exit,
            AuthorityThreadExit::BootstrapError(ref message)
                if message == "fixture bootstrap rejected"
        ));
        assert_eq!(terminal.last_tick, SimTick::ZERO);
        assert_eq!(terminal.metrics.simulated_ticks, 0);
        handle.join().unwrap();
    }

    #[test]
    fn stalled_report_consumer_does_not_change_authority_ticks() {
        let fast_ticks = Arc::new(Mutex::new(Vec::new()));
        let stalled_ticks = Arc::new(Mutex::new(Vec::new()));
        let mut fast = spawn_authority_thread(
            authority(Some(6), Arc::clone(&fast_ticks)),
            AuthorityThreadConfig::default(),
        )
        .unwrap();
        let stalled = spawn_authority_thread(
            authority(Some(6), Arc::clone(&stalled_ticks)),
            AuthorityThreadConfig::default(),
        )
        .unwrap();

        while fast.final_result().is_none() {
            let _ = fast
                .reports_mut()
                .recv_latest_timeout(Duration::from_millis(100));
        }
        let fast_terminal = fast.wait_for_terminal(Duration::from_secs(2)).unwrap();
        let stalled_terminal = stalled.wait_for_terminal(Duration::from_secs(2)).unwrap();
        assert!(matches!(
            fast_terminal.exit,
            AuthorityThreadExit::MatchFinished { result_id: 77 }
        ));
        assert!(matches!(
            stalled_terminal.exit,
            AuthorityThreadExit::MatchFinished { result_id: 77 }
        ));
        assert_eq!(*fast_ticks.lock().unwrap(), *stalled_ticks.lock().unwrap());
        assert_eq!(
            *stalled_ticks.lock().unwrap(),
            (1..=6).map(SimTick).collect::<Vec<_>>()
        );
        assert!(stalled.metrics().tick_reports_coalesced > 0);
        assert_eq!(stalled.reports.latest().unwrap().tick, SimTick(6));
        fast.join().unwrap();
        stalled.join().unwrap();
    }

    #[test]
    fn stop_and_drop_shutdown_cleanly() {
        let ticks = Arc::new(Mutex::new(Vec::new()));
        let handle = spawn_authority_thread(
            authority(None, Arc::clone(&ticks)),
            AuthorityThreadConfig::default(),
        )
        .unwrap();
        assert_eq!(handle.try_stop(), AuthorityCommandSubmitOutcome::Queued);
        let terminal = handle.wait_for_terminal(Duration::from_secs(1)).unwrap();
        assert!(matches!(terminal.exit, AuthorityThreadExit::StopRequested));
        handle.join().unwrap();
        let completed = ticks.lock().unwrap().len();
        thread::sleep(Duration::from_millis(30));
        assert_eq!(ticks.lock().unwrap().len(), completed);

        let dropped_ticks = Arc::new(Mutex::new(Vec::new()));
        {
            let _handle = spawn_authority_thread(
                authority(None, Arc::clone(&dropped_ticks)),
                AuthorityThreadConfig::default(),
            )
            .unwrap();
        }
        let completed = dropped_ticks.lock().unwrap().len();
        thread::sleep(Duration::from_millis(30));
        assert_eq!(dropped_ticks.lock().unwrap().len(), completed);
    }

    #[test]
    fn manual_runner_is_unpaced_and_contiguous() {
        let ticks = Arc::new(Mutex::new(Vec::new()));
        let mut runner = ManualAuthorityRunner::new(authority(None, Arc::clone(&ticks)));
        let started = Instant::now();
        runner.run_ticks(600, |_| {}).unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(runner.simulated_ticks(), 600);
        assert_eq!(ticks.lock().unwrap().last(), Some(&SimTick(600)));
    }
}
