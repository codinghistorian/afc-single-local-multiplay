//! Single-threaded production client for browser-hosted AFC matches.
//!
//! Browsers cannot run the native remote client's socket/rollback worker model
//! without opting into cross-origin-isolated Web Workers. This owner keeps the
//! exact transport-independent predicted protocol and predicted Bevy world on
//! the calling thread instead. The caller supplies a monotonic browser clock;
//! a rational 60 Hz scheduler performs bounded catch-up without dropping fixed
//! ticks. Transport callbacks feed a bounded [`NonBlockingDatagramEndpoint`]
//! adapter and never mutate simulation state directly.

use bevy::prelude::World;
use core::fmt;
use std::collections::VecDeque;
use std::marker::PhantomData;
use std::rc::Rc;

use crate::authority_thread::AUTHORITY_THREAD_TICK_RATE_HZ;
use crate::client_protocol::{
    ClientProtocolBuildError, ClientProtocolConfig, ClientProtocolError, ClientProtocolFatalError,
    ClientProtocolFault, ClientProtocolRecoverableError, ClientProtocolTime,
    RemotePredictedClientProtocol,
};
use crate::confirmed_progression::ConfirmedProgressionLedger;
use crate::headless::{HeadlessBuildError, HeadlessMatchConfig, build_predicted_simulation};
use crate::live_authority::{LiveSimulationDriver, LiveSimulationError};
use crate::live_input::local_tick_to_network_input;
use crate::match_presentation::ConfirmedMatchPresentation;
use crate::network_io::NonBlockingDatagramEndpoint;
use crate::network_protocol::{
    ConnectionPhase, InputButtons, InputFrame, InputSequence, MAX_SEATS, MatchManifest, PeerId,
    SeatId, SeatOwner, SimTick,
};
use crate::network_quality::{
    NetworkQualityError, NetworkQualityMonitor, NetworkQualityPolicy, NetworkQualitySample,
    NetworkQualitySnapshot,
};
use crate::network_runtime::RuntimeConnectionState;
use crate::online_failure::{
    OnlineFailure, OnlineFailureCode, OnlineFailureSeverity, OnlineRecoveryAction,
};
use crate::predicted_client::{PredictedClient, PredictedClientError};
use crate::presentation_projection::LivePresentationProjector;
use crate::remote_online_client::{
    RemoteAuthorityDisconnect, RemoteLocalInputBatch, RemoteLocalInputSample,
    RemoteOnlineClientPhase, RemoteOnlineClientStatus, RemoteOnlineClientUpdate,
    RemoteOnlinePresentationError, RemoteOnlineTerminal, RemoteOnlineWorkerMetrics,
    RemotePresentationTick, RemoteProjectionFrame, WorkerRollbackHooks, discard_projection_after,
    install_presentation_tick, projection_source_world,
};
use crate::rollback::{InstantRollbackTiming, RollbackMetrics};
use crate::session::ConfirmedSessionResult;
use crate::sim_event::{SIM_EVENT_HISTORY_TICKS, SimEventJournal};
use crate::tick_input::{LocalSeatId, LocalTickInputState};

pub const DEFAULT_BROWSER_FIXED_STEPS_PER_SERVICE: u16 = 8;
pub const MAX_BROWSER_FIXED_STEPS_PER_SERVICE: u16 = 64;
const MICROS_PER_SECOND: u64 = 1_000_000;

type LiveBrowserProtocol<E> = RemotePredictedClientProtocol<
    E,
    LiveSimulationDriver,
    WorkerRollbackHooks,
    InstantRollbackTiming,
>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BrowserOnlineClientConfig {
    pub protocol: ClientProtocolConfig,
    pub quality_policy: NetworkQualityPolicy,
    /// Maximum fixed ticks executed by one animation-frame service call. Due
    /// ticks beyond this limit remain queued; they are never skipped.
    pub max_fixed_steps_per_service: u16,
}

impl Default for BrowserOnlineClientConfig {
    fn default() -> Self {
        Self {
            protocol: ClientProtocolConfig::default(),
            quality_policy: NetworkQualityPolicy::default(),
            max_fixed_steps_per_service: DEFAULT_BROWSER_FIXED_STEPS_PER_SERVICE,
        }
    }
}

impl BrowserOnlineClientConfig {
    pub fn validate(self) -> Result<(), BrowserOnlineClientConfigError> {
        self.protocol
            .validate()
            .map_err(BrowserOnlineClientConfigError::Protocol)?;
        self.quality_policy
            .validate()
            .map_err(BrowserOnlineClientConfigError::Quality)?;
        if self.max_fixed_steps_per_service == 0
            || self.max_fixed_steps_per_service > MAX_BROWSER_FIXED_STEPS_PER_SERVICE
        {
            return Err(BrowserOnlineClientConfigError::FixedStepLimit);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserOnlineClientConfigError {
    Protocol(ClientProtocolBuildError),
    Quality(NetworkQualityError),
    FixedStepLimit,
}

impl fmt::Display for BrowserOnlineClientConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid browser online client configuration: {self:?}"
        )
    }
}

impl std::error::Error for BrowserOnlineClientConfigError {}

#[derive(Debug)]
pub enum BrowserOnlineClientStartError {
    InvalidConfig(BrowserOnlineClientConfigError),
    InvalidMatch(HeadlessBuildError),
    Prediction(PredictedClientError<LiveSimulationError>),
    Protocol(ClientProtocolBuildError),
    PeerOwnsNoSeat,
    ReconnectBoundaryUnavailable,
    GenerationExhausted,
    TickRateMismatch { manifest_hz: u16 },
}

impl fmt::Display for BrowserOnlineClientStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "browser online client could not start: {self:?}")
    }
}

impl std::error::Error for BrowserOnlineClientStartError {}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BrowserOnlineClientMetrics {
    pub service_calls: u64,
    pub fixed_steps: u64,
    pub catch_up_limited_service_calls: u64,
    pub fixed_tick_backlog_high_water: u64,
    pub input_batches_submitted: u64,
    pub quality_samples_submitted: u64,
    pub input_ticks_submitted: u64,
    pub input_backpressure_retries: u64,
    pub snapshots_published: u64,
    pub snapshots_coalesced: u64,
    pub dropped_presentation_event_ticks: u64,
}

impl BrowserOnlineClientMetrics {
    fn remote_worker_view(self) -> RemoteOnlineWorkerMetrics {
        RemoteOnlineWorkerMetrics {
            input_commands_submitted: self.input_batches_submitted,
            quality_commands_submitted: self.quality_samples_submitted,
            commands_processed: self
                .input_batches_submitted
                .saturating_add(self.quality_samples_submitted),
            worker_iterations: self.fixed_steps,
            input_ticks_submitted: self.input_ticks_submitted,
            input_backpressure_retries: self.input_backpressure_retries,
            snapshots_published: self.snapshots_published,
            snapshots_coalesced: self.snapshots_coalesced,
            dropped_presentation_event_ticks: self.dropped_presentation_event_ticks,
            ..RemoteOnlineWorkerMetrics::default()
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BrowserOnlineServiceReport {
    pub fixed_steps: u16,
    pub pending_fixed_ticks: u64,
    pub status: RemoteOnlineClientStatus,
    pub terminal: Option<RemoteOnlineTerminal>,
}

#[derive(Debug)]
pub enum BrowserLocalInputError {
    Protocol(crate::network_protocol::ProtocolValidationError),
    Rejected(OnlineFailure),
}

impl fmt::Display for BrowserLocalInputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "browser local input was rejected: {self:?}")
    }
}

impl std::error::Error for BrowserLocalInputError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BrowserBootstrapMode {
    Initial,
    Reconnect { countdown_start_tick: SimTick },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BrowserClientFailure {
    Generic(OnlineFailure),
    AuthorityDisconnect(RemoteAuthorityDisconnect),
}

impl From<OnlineFailure> for BrowserClientFailure {
    fn from(failure: OnlineFailure) -> Self {
        Self::Generic(failure)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct BrowserSeatInput {
    latest: Option<RemoteLocalInputSample>,
    next_sequence: InputSequence,
}

impl BrowserSeatInput {
    fn merge(&mut self, sample: RemoteLocalInputSample) {
        self.latest = Some(match self.latest {
            Some(mut pending) => {
                pending.movement_x = sample.movement_x;
                pending.movement_y = sample.movement_y;
                pending.held_buttons = sample.held_buttons;
                pending.pressed_buttons = InputButtons::new(
                    pending.pressed_buttons.bits() | sample.pressed_buttons.bits(),
                )
                .expect("merging supported input edges stays supported");
                pending.released_buttons = InputButtons::new(
                    pending.released_buttons.bits() | sample.released_buttons.bits(),
                )
                .expect("merging supported input edges stays supported");
                pending
            }
            None => sample,
        });
    }

    fn frame_for_tick(&mut self, tick: SimTick, seat: SeatId) -> InputFrame {
        let sample = self.latest.unwrap_or(RemoteLocalInputSample {
            seat,
            ..RemoteLocalInputSample::default()
        });
        let frame = InputFrame {
            tick,
            seat,
            movement_x: sample.movement_x,
            movement_y: sample.movement_y,
            held_buttons: sample.held_buttons,
            pressed_buttons: sample.pressed_buttons,
            released_buttons: sample.released_buttons,
            sequence: self.next_sequence,
        };
        self.next_sequence = InputSequence(self.next_sequence.0.wrapping_add(1));
        if let Some(latest) = self.latest.as_mut() {
            latest.pressed_buttons = InputButtons::default();
            latest.released_buttons = InputButtons::default();
        }
        frame
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct BrowserFixedTickClock {
    epoch_micros: Option<u64>,
    last_observed_micros: Option<u64>,
    serviced_ticks: u64,
}

impl BrowserFixedTickClock {
    fn observe(&mut self, monotonic_micros: u64) -> Result<u64, ()> {
        if self
            .last_observed_micros
            .is_some_and(|previous| monotonic_micros < previous)
        {
            return Err(());
        }
        self.last_observed_micros = Some(monotonic_micros);
        let epoch = *self.epoch_micros.get_or_insert(monotonic_micros);
        let elapsed = monotonic_micros.saturating_sub(epoch);
        let total_due = (u128::from(elapsed) * u128::from(AUTHORITY_THREAD_TICK_RATE_HZ)
            / u128::from(MICROS_PER_SECOND))
        .saturating_add(1)
        .min(u128::from(u64::MAX)) as u64;
        Ok(total_due.saturating_sub(self.serviced_ticks))
    }

    fn next_time(&self) -> Result<ClientProtocolTime, OnlineFailure> {
        let epoch = self.epoch_micros.ok_or_else(clock_failure)?;
        let network_tick = self
            .serviced_ticks
            .checked_add(1)
            .map(SimTick)
            .ok_or_else(clock_failure)?;
        let offset = (u128::from(self.serviced_ticks) * u128::from(MICROS_PER_SECOND)
            / u128::from(AUTHORITY_THREAD_TICK_RATE_HZ))
        .min(u128::from(u64::MAX)) as u64;
        Ok(ClientProtocolTime {
            network_tick,
            monotonic_micros: epoch.checked_add(offset).ok_or_else(clock_failure)?,
        })
    }

    fn commit_tick(&mut self) {
        self.serviced_ticks = self.serviced_ticks.saturating_add(1);
    }
}

#[derive(Default)]
struct BrowserProjectionMailbox {
    frame: Option<RemoteProjectionFrame>,
    event_ticks: VecDeque<RemotePresentationTick>,
    rollback_retain_through: Option<SimTick>,
    confirmed_result: Option<ConfirmedSessionResult>,
}

/// Main-thread-only owner for one browser predicted client.
///
/// The `Rc` marker is deliberate: even on native test targets this value is
/// neither `Send` nor `Sync`, preventing an accidental second simulation
/// worker from diverging from browser execution semantics.
pub struct BrowserOnlineClient<E>
where
    E: NonBlockingDatagramEndpoint,
{
    match_config: HeadlessMatchConfig,
    peer_id: PeerId,
    config: BrowserOnlineClientConfig,
    generation: u64,
    mode: BrowserBootstrapMode,
    protocol: LiveBrowserProtocol<E>,
    rollback: WorkerRollbackHooks,
    quality: NetworkQualityMonitor,
    inputs: [BrowserSeatInput; MAX_SEATS],
    pending_input_ticks: Option<(SimTick, SimTick)>,
    content_ready: bool,
    content_loaded: bool,
    last_published_tick: Option<SimTick>,
    confirmed_result: Option<ConfirmedSessionResult>,
    gameplay_link_retired: bool,
    last_clock_samples: u64,
    clock: BrowserFixedTickClock,
    status: RemoteOnlineClientStatus,
    terminal: Option<RemoteOnlineTerminal>,
    metrics: BrowserOnlineClientMetrics,
    projection_mailbox: BrowserProjectionMailbox,
    projector: LivePresentationProjector,
    projection_source: World,
    presentation_prepared: bool,
    sample_tick: u64,
    pending_local_samples: [Option<RemoteLocalInputSample>; MAX_SEATS],
    stopped: bool,
    _main_thread_only: PhantomData<Rc<()>>,
}

impl<E> BrowserOnlineClient<E>
where
    E: NonBlockingDatagramEndpoint,
{
    pub fn new(
        endpoint: E,
        match_config: HeadlessMatchConfig,
        peer_id: PeerId,
        config: BrowserOnlineClientConfig,
    ) -> Result<Self, BrowserOnlineClientStartError> {
        Self::new_inner(
            endpoint,
            match_config,
            peer_id,
            config,
            1,
            BrowserBootstrapMode::Initial,
        )
    }

    fn new_inner(
        endpoint: E,
        match_config: HeadlessMatchConfig,
        peer_id: PeerId,
        config: BrowserOnlineClientConfig,
        generation: u64,
        mode: BrowserBootstrapMode,
    ) -> Result<Self, BrowserOnlineClientStartError> {
        validate_bootstrap(&match_config, peer_id, config)?;
        let predicted = build_predicted_simulation(match_config.clone())
            .map_err(BrowserOnlineClientStartError::InvalidMatch)?;
        let rollback = WorkerRollbackHooks::default();
        let predicted = PredictedClient::with_hooks(
            predicted,
            match_config.manifest.match_id,
            usize::from(match_config.manifest.snapshot_history_ticks),
            rollback.clone(),
            InstantRollbackTiming::default(),
        )
        .map_err(BrowserOnlineClientStartError::Prediction)?;
        let initial_time = ClientProtocolTime::default();
        let protocol = match mode {
            BrowserBootstrapMode::Initial => RemotePredictedClientProtocol::new(
                endpoint,
                match_config.manifest.match_id,
                peer_id,
                match_config.manifest.compatibility,
                predicted,
                config.protocol,
                initial_time,
            ),
            BrowserBootstrapMode::Reconnect {
                countdown_start_tick,
            } => RemotePredictedClientProtocol::new_reconnect(
                endpoint,
                match_config.manifest,
                peer_id,
                countdown_start_tick,
                predicted,
                config.protocol,
                initial_time,
            ),
        }
        .map_err(BrowserOnlineClientStartError::Protocol)?;
        let quality = NetworkQualityMonitor::new(config.quality_policy).map_err(|error| {
            BrowserOnlineClientStartError::InvalidConfig(BrowserOnlineClientConfigError::Quality(
                error,
            ))
        })?;
        let reconnecting = matches!(mode, BrowserBootstrapMode::Reconnect { .. });
        Ok(Self {
            match_config,
            peer_id,
            config,
            generation,
            mode,
            protocol,
            rollback,
            quality,
            inputs: [BrowserSeatInput::default(); MAX_SEATS],
            pending_input_ticks: None,
            content_ready: reconnecting,
            content_loaded: reconnecting,
            last_published_tick: None,
            confirmed_result: None,
            gameplay_link_retired: false,
            last_clock_samples: 0,
            clock: BrowserFixedTickClock::default(),
            status: RemoteOnlineClientStatus {
                generation,
                phase: if reconnecting {
                    RemoteOnlineClientPhase::Reconnecting
                } else {
                    RemoteOnlineClientPhase::Connecting
                },
                ..RemoteOnlineClientStatus::default()
            },
            terminal: None,
            metrics: BrowserOnlineClientMetrics::default(),
            projection_mailbox: BrowserProjectionMailbox::default(),
            projector: LivePresentationProjector::new(),
            projection_source: projection_source_world(),
            presentation_prepared: false,
            sample_tick: 0,
            pending_local_samples: [None; MAX_SEATS],
            stopped: false,
            _main_thread_only: PhantomData,
        })
    }

    pub const fn manifest(&self) -> &MatchManifest {
        &self.match_config.manifest
    }

    pub const fn peer_id(&self) -> PeerId {
        self.peer_id
    }

    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub const fn status(&self) -> RemoteOnlineClientStatus {
        self.status
    }

    pub const fn confirmed_result(&self) -> Option<ConfirmedSessionResult> {
        self.confirmed_result
    }

    pub const fn terminal(&self) -> Option<RemoteOnlineTerminal> {
        self.terminal
    }

    pub const fn metrics(&self) -> BrowserOnlineClientMetrics {
        self.metrics
    }

    pub fn mark_content_loaded(&mut self) {
        self.content_ready = true;
    }

    pub fn submit_inputs(&mut self, batch: RemoteLocalInputBatch) -> Result<(), OnlineFailure> {
        if self.stopped
            || self
                .terminal
                .is_some_and(|terminal| !matches!(terminal, RemoteOnlineTerminal::Completed(_)))
        {
            return Err(connection_failure());
        }
        let manifest = self.match_config.manifest;
        apply_input_batch(&mut self.inputs, batch, &manifest, self.peer_id)?;
        self.metrics.input_batches_submitted =
            self.metrics.input_batches_submitted.saturating_add(1);
        Ok(())
    }

    pub fn submit_quality_sample(
        &mut self,
        sample: NetworkQualitySample,
    ) -> Result<(), NetworkQualityError> {
        self.quality.observe(sample)?;
        self.metrics.quality_samples_submitted =
            self.metrics.quality_samples_submitted.saturating_add(1);
        Ok(())
    }

    /// Drains one render-frame action sample for every seat owned by this
    /// browser guest. Edges are merged until accepted by a fixed simulation
    /// tick, so low frame rate or transport pressure cannot consume a press.
    pub fn sample_local_inputs(
        &mut self,
        local_inputs: &mut LocalTickInputState,
    ) -> Result<(), BrowserLocalInputError> {
        self.sample_tick = self.sample_tick.saturating_add(1);
        let manifest = *self.manifest();
        let peer_id = self.peer_id;
        let mut local_index = 0;
        for assignment in manifest.ownership.as_slice() {
            if assignment.owner != SeatOwner::Peer(peer_id) {
                continue;
            }
            let local_seat =
                LocalSeatId::new(local_index).ok_or(BrowserLocalInputError::Protocol(
                    crate::network_protocol::ProtocolValidationError::InvalidSeat,
                ))?;
            let raw = local_inputs.drain_for_tick(local_seat, self.sample_tick);
            let action = local_tick_to_network_input(raw, local_inputs.gestures_mut(local_seat));
            let mut sample = RemoteLocalInputSample::from_action_frame(action);
            sample.seat = assignment.seat;
            merge_local_sample(
                &mut self.pending_local_samples[usize::from(assignment.seat.get())],
                sample,
            );
            local_index += 1;
        }
        let mut samples = [RemoteLocalInputSample::default(); MAX_SEATS];
        let mut count = 0;
        for assignment in manifest.ownership.as_slice() {
            if assignment.owner != SeatOwner::Peer(peer_id) {
                continue;
            }
            let Some(sample) = self.pending_local_samples[usize::from(assignment.seat.get())]
            else {
                return Err(BrowserLocalInputError::Protocol(
                    crate::network_protocol::ProtocolValidationError::InvalidLocalSeatCount,
                ));
            };
            samples[count] = sample;
            count += 1;
        }
        let batch = RemoteLocalInputBatch::new(&samples[..count])
            .map_err(BrowserLocalInputError::Protocol)?;
        self.submit_inputs(batch)
            .map_err(BrowserLocalInputError::Rejected)?;
        for assignment in manifest.ownership.as_slice() {
            if assignment.owner == SeatOwner::Peer(peer_id) {
                self.pending_local_samples[usize::from(assignment.seat.get())] = None;
            }
        }
        Ok(())
    }

    /// Services every due fixed tick up to the configured per-frame bound.
    /// `monotonic_micros` should come from `performance.now()` converted to an
    /// integer; a regression fails closed through the ordinary online status.
    pub fn service(&mut self, monotonic_micros: u64) -> BrowserOnlineServiceReport {
        self.metrics.service_calls = self.metrics.service_calls.saturating_add(1);
        if self.stopped
            || self
                .terminal
                .is_some_and(|terminal| !matches!(terminal, RemoteOnlineTerminal::Completed(_)))
        {
            return BrowserOnlineServiceReport {
                status: self.status,
                terminal: self.terminal,
                ..BrowserOnlineServiceReport::default()
            };
        }
        let due = match self.clock.observe(monotonic_micros) {
            Ok(due) => due,
            Err(()) => {
                self.finish(BrowserClientFailure::Generic(clock_failure()));
                return BrowserOnlineServiceReport {
                    status: self.status,
                    terminal: self.terminal,
                    ..BrowserOnlineServiceReport::default()
                };
            }
        };
        let fixed_steps = due.min(u64::from(self.config.max_fixed_steps_per_service)) as u16;
        if due > u64::from(fixed_steps) {
            self.metrics.catch_up_limited_service_calls = self
                .metrics
                .catch_up_limited_service_calls
                .saturating_add(1);
        }
        self.metrics.fixed_tick_backlog_high_water =
            self.metrics.fixed_tick_backlog_high_water.max(due);

        let mut serviced = 0_u16;
        for _ in 0..fixed_steps {
            let now = match self.clock.next_time() {
                Ok(now) => now,
                Err(failure) => {
                    self.finish(BrowserClientFailure::Generic(failure));
                    break;
                }
            };
            self.clock.commit_tick();
            self.metrics.fixed_steps = self.metrics.fixed_steps.saturating_add(1);
            serviced = serviced.saturating_add(1);
            if let Err(failure) = self.service_fixed_tick(now) {
                self.finish(failure);
                break;
            }
        }
        let pending_fixed_ticks = self.clock.observe(monotonic_micros).unwrap_or_default();
        BrowserOnlineServiceReport {
            fixed_steps: serviced,
            pending_fixed_ticks,
            status: self.status,
            terminal: self.terminal,
        }
    }

    fn service_fixed_tick(&mut self, now: ClientProtocolTime) -> Result<(), BrowserClientFailure> {
        if self.gameplay_link_retired {
            self.status.network_tick = now.network_tick;
            self.status.worker = self.metrics.remote_worker_view();
            return Ok(());
        }

        let local_confirmed_tick = self.protocol.predicted_client().confirmed_tick();
        if let Err(error) = self.protocol.pump(now) {
            if self.confirmed_result.is_some() && is_post_result_transport_retirement(&error) {
                self.gameplay_link_retired = true;
                self.status.network_tick = now.network_tick;
                self.status.worker = self.metrics.remote_worker_view();
                return Ok(());
            }
            return Err(map_protocol_error(
                error,
                self.generation,
                local_confirmed_tick,
            ));
        }
        if let Some(received) = self.protocol.manifest().copied() {
            if received != self.match_config.manifest {
                return Err(incompatible_failure().into());
            }
            if !self.content_loaded && self.content_ready {
                self.protocol.mark_content_loaded(now).map_err(|error| {
                    map_protocol_error(
                        error,
                        self.generation,
                        self.protocol.predicted_client().confirmed_tick(),
                    )
                })?;
                self.content_loaded = true;
            }
        }

        let submitted = if self.confirmed_result.is_none() {
            service_due_inputs(
                &mut self.protocol,
                &mut self.inputs,
                &mut self.pending_input_ticks,
                &mut self.metrics,
                self.generation,
            )?
        } else {
            false
        };
        if submitted {
            let local_confirmed_tick = self.protocol.predicted_client().confirmed_tick();
            self.protocol.pump(now).map_err(|error| {
                map_protocol_error(error, self.generation, local_confirmed_tick)
            })?;
        }

        let clock_metrics = self.protocol.session_clock_metrics();
        if clock_metrics.accepted_samples != self.last_clock_samples {
            self.last_clock_samples = clock_metrics.accepted_samples;
            let rtt_ms = clock_metrics
                .best_rtt_micros
                .saturating_add(999)
                .saturating_div(1_000)
                .min(u32::from(u16::MAX)) as u16;
            self.quality
                .observe(NetworkQualitySample {
                    rtt_ms,
                    loss_bps: 0,
                })
                .map_err(|_| internal_failure())?;
        }

        let confirmed_result = self.protocol.take_confirmed_result();
        self.status = make_status(
            self.generation,
            self.mode,
            &self.protocol,
            now.network_tick,
            self.quality.snapshot(),
            self.metrics.remote_worker_view(),
        );
        self.capture_projection(confirmed_result)?;
        if let Some(result) = confirmed_result {
            self.confirmed_result = Some(result);
            self.projection_mailbox.confirmed_result = Some(result);
            self.status.phase = RemoteOnlineClientPhase::Results;
            self.status.failure = None;
            self.status.authority_disconnect = None;
            self.terminal = Some(RemoteOnlineTerminal::Completed(result));
        }
        Ok(())
    }

    fn capture_projection(
        &mut self,
        confirmed_result: Option<ConfirmedSessionResult>,
    ) -> Result<(), OnlineFailure> {
        let client = self.protocol.predicted_client();
        let Some(predicted_tick) = client.predicted_tick() else {
            return if confirmed_result.is_some() {
                Err(synchronization_failure())
            } else {
                Ok(())
            };
        };
        let target_tick = confirmed_result
            .map(|result| result.final_tick)
            .unwrap_or(predicted_tick);
        let mut rollback_retain_through = self.rollback.take();
        if confirmed_result.is_some() && target_tick < predicted_tick {
            rollback_retain_through = Some(
                rollback_retain_through.map_or(target_tick, |pending| pending.min(target_tick)),
            );
        }
        if confirmed_result.is_none()
            && rollback_retain_through.is_none()
            && self.last_published_tick == Some(target_tick)
        {
            return Ok(());
        }
        let source = client.world().world();
        let journal = source
            .get_resource::<SimEventJournal>()
            .ok_or_else(internal_failure)?;
        let start = rollback_retain_through
            .and_then(|tick| tick.0.checked_add(1).map(SimTick))
            .or_else(|| {
                self.last_published_tick
                    .and_then(|tick| tick.0.checked_add(1).map(SimTick))
            })
            .or_else(|| journal.oldest_tick());
        let mut event_ticks = Vec::new();
        if let Some(mut tick) = start {
            while tick <= target_tick {
                if let Some(events) = RemotePresentationTick::capture(source, tick)? {
                    event_ticks.push(events);
                }
                if tick == target_tick {
                    break;
                }
                tick = tick.next();
            }
        }
        let snapshot = if confirmed_result.is_some() {
            client
                .snapshot_at(target_tick)
                .cloned()
                .ok_or_else(synchronization_failure)?
        } else {
            client
                .world()
                .capture_live_snapshot()
                .map_err(|_| synchronization_failure())?
        };
        self.publish_projection(
            RemoteProjectionFrame {
                snapshot,
                confirmed_through: confirmed_result
                    .map(|result| result.final_tick)
                    .or_else(|| client.confirmed_tick()),
                confirmed_result,
            },
            rollback_retain_through,
            event_ticks,
        );
        self.last_published_tick = Some(target_tick);
        Ok(())
    }

    fn publish_projection(
        &mut self,
        frame: RemoteProjectionFrame,
        rollback_retain_through: Option<SimTick>,
        event_ticks: Vec<RemotePresentationTick>,
    ) {
        if self.projection_mailbox.frame.is_some() {
            self.metrics.snapshots_coalesced = self.metrics.snapshots_coalesced.saturating_add(1);
        }
        if let Some(retained_through) = rollback_retain_through {
            self.projection_mailbox
                .event_ticks
                .retain(|events| events.tick <= retained_through);
            self.projection_mailbox.rollback_retain_through = Some(
                self.projection_mailbox
                    .rollback_retain_through
                    .map_or(retained_through, |pending| pending.min(retained_through)),
            );
        }
        for events in event_ticks {
            if let Some(existing) = self
                .projection_mailbox
                .event_ticks
                .iter_mut()
                .find(|queued| queued.tick == events.tick)
            {
                *existing = events;
            } else {
                self.projection_mailbox.event_ticks.push_back(events);
            }
            while self.projection_mailbox.event_ticks.len() > SIM_EVENT_HISTORY_TICKS {
                self.projection_mailbox.event_ticks.pop_front();
                self.metrics.dropped_presentation_event_ticks = self
                    .metrics
                    .dropped_presentation_event_ticks
                    .saturating_add(1);
            }
        }
        if let Some(result) = frame.confirmed_result {
            if let Some(existing) = self.projection_mailbox.confirmed_result {
                assert_eq!(existing, result, "one generation has one confirmed result");
            }
            self.projection_mailbox.confirmed_result = Some(result);
        }
        self.projection_mailbox.frame = Some(frame);
        self.metrics.snapshots_published = self.metrics.snapshots_published.saturating_add(1);
    }

    pub fn prepare_presentation_target(
        &mut self,
        target: &mut World,
    ) -> Result<(), RemoteOnlinePresentationError> {
        if !target.contains_resource::<ConfirmedProgressionLedger>() {
            target.insert_resource(ConfirmedProgressionLedger::default());
        }
        let manifest = self.match_config.manifest;
        self.projector
            .prepare_target(target, &manifest)
            .map_err(RemoteOnlinePresentationError::Projection)?;
        self.presentation_prepared = true;
        Ok(())
    }

    pub fn project_latest(
        &mut self,
        target: &mut World,
    ) -> Result<RemoteOnlineClientUpdate, RemoteOnlinePresentationError> {
        if !self.presentation_prepared {
            self.prepare_presentation_target(target)?;
        }
        if let Some(retained_through) = self.projection_mailbox.rollback_retain_through.take() {
            discard_projection_after(
                &mut self.projection_source,
                &mut self.projector,
                retained_through,
            );
        }
        for events in self.projection_mailbox.event_ticks.drain(..) {
            install_presentation_tick(&mut self.projection_source, events)?;
        }

        let mut projected_confirmed_result = None;
        let projection = if let Some(frame) = self.projection_mailbox.frame.take() {
            let prepared = if let Some(result) = frame.confirmed_result {
                Some(
                    target
                        .resource_mut::<ConfirmedProgressionLedger>()
                        .prepare_observation(self.manifest(), result, &frame.snapshot, true)
                        .map_err(RemoteOnlinePresentationError::Progression)?,
                )
            } else {
                None
            };
            let report = self
                .projector
                .project_snapshot(
                    &self.projection_source,
                    &frame.snapshot,
                    target,
                    frame.confirmed_through,
                )
                .map_err(RemoteOnlinePresentationError::Projection)?;
            if let Some(prepared) = prepared {
                let presentation = ConfirmedMatchPresentation::from_confirmed_record(
                    self.manifest(),
                    self.peer_id,
                    prepared.record(),
                );
                target
                    .resource_mut::<ConfirmedProgressionLedger>()
                    .commit_prepared(prepared);
                target.insert_resource(presentation);
                projected_confirmed_result = frame.confirmed_result;
            }
            Some(report)
        } else {
            None
        };

        Ok(RemoteOnlineClientUpdate {
            projection,
            status: self.status,
            confirmed_result: self.confirmed_result,
            projected_confirmed_result,
            terminal: self.terminal,
        })
    }

    pub fn reconnect(&mut self, endpoint: E) -> Result<(), BrowserOnlineClientStartError> {
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(BrowserOnlineClientStartError::GenerationExhausted)?;
        let countdown_start_tick = self
            .status
            .countdown_start_tick
            .ok_or(BrowserOnlineClientStartError::ReconnectBoundaryUnavailable)?;
        let replacement = Self::new_inner(
            endpoint,
            self.match_config.clone(),
            self.peer_id,
            self.config,
            generation,
            BrowserBootstrapMode::Reconnect {
                countdown_start_tick,
            },
        )?;
        *self = replacement;
        Ok(())
    }

    pub fn stop(&mut self) {
        self.stopped = true;
        if !matches!(self.terminal, Some(RemoteOnlineTerminal::Completed(_))) {
            self.status.phase = RemoteOnlineClientPhase::Stopped;
            self.status.failure = None;
            self.status.authority_disconnect = None;
            self.terminal = Some(RemoteOnlineTerminal::Stopped);
        }
    }

    fn finish(&mut self, failure: BrowserClientFailure) {
        match failure {
            BrowserClientFailure::Generic(failure) => {
                self.status.failure = Some(failure);
                self.status.phase = RemoteOnlineClientPhase::Failed;
                self.status.authority_disconnect = None;
                self.terminal = Some(RemoteOnlineTerminal::Failed(failure));
            }
            BrowserClientFailure::AuthorityDisconnect(disconnect) => {
                let failure = OnlineFailure::from_disconnect(disconnect.message);
                self.status.failure = Some(failure);
                self.status.authority_disconnect = Some(disconnect);
                self.status.phase = match failure.recovery {
                    OnlineRecoveryAction::Reconnect => RemoteOnlineClientPhase::Reconnecting,
                    OnlineRecoveryAction::MatchEndedNoContest => RemoteOnlineClientPhase::Results,
                    OnlineRecoveryAction::Dismiss
                    | OnlineRecoveryAction::Retry
                    | OnlineRecoveryAction::ReturnToLobby
                    | OnlineRecoveryAction::ReturnToMenu
                    | OnlineRecoveryAction::DisableOnline => RemoteOnlineClientPhase::Failed,
                };
                self.terminal = Some(RemoteOnlineTerminal::AuthorityDisconnected(disconnect));
            }
        }
    }
}

fn validate_bootstrap(
    match_config: &HeadlessMatchConfig,
    peer_id: PeerId,
    config: BrowserOnlineClientConfig,
) -> Result<(), BrowserOnlineClientStartError> {
    config
        .validate()
        .map_err(BrowserOnlineClientStartError::InvalidConfig)?;
    match_config
        .validate()
        .map_err(BrowserOnlineClientStartError::InvalidMatch)?;
    if u32::from(match_config.manifest.tick_rate_hz) != AUTHORITY_THREAD_TICK_RATE_HZ {
        return Err(BrowserOnlineClientStartError::TickRateMismatch {
            manifest_hz: match_config.manifest.tick_rate_hz,
        });
    }
    if !match_config.manifest.ownership.peer_owns_any_seat(peer_id) {
        return Err(BrowserOnlineClientStartError::PeerOwnsNoSeat);
    }
    Ok(())
}

fn merge_local_sample(
    pending: &mut Option<RemoteLocalInputSample>,
    sample: RemoteLocalInputSample,
) {
    match pending {
        Some(pending) => {
            pending.movement_x = sample.movement_x;
            pending.movement_y = sample.movement_y;
            pending.held_buttons = sample.held_buttons;
            pending.pressed_buttons =
                InputButtons::new(pending.pressed_buttons.bits() | sample.pressed_buttons.bits())
                    .expect("merging supported local input edges stays supported");
            pending.released_buttons =
                InputButtons::new(pending.released_buttons.bits() | sample.released_buttons.bits())
                    .expect("merging supported local input edges stays supported");
        }
        None => *pending = Some(sample),
    }
}

fn apply_input_batch(
    inputs: &mut [BrowserSeatInput; MAX_SEATS],
    batch: RemoteLocalInputBatch,
    manifest: &MatchManifest,
    peer_id: PeerId,
) -> Result<(), OnlineFailure> {
    let expected = manifest
        .ownership
        .as_slice()
        .iter()
        .filter(|assignment| assignment.owner == SeatOwner::Peer(peer_id))
        .count();
    if batch.len() != expected {
        return Err(invalid_input_failure());
    }
    let mut staged = *inputs;
    let mut seen = 0_u8;
    for sample in batch.iter() {
        sample.validate().map_err(|_| invalid_input_failure())?;
        manifest
            .ownership
            .validate_peer_input(peer_id, sample.seat)
            .map_err(|_| invalid_input_failure())?;
        let bit = 1 << sample.seat.get();
        if seen & bit != 0 {
            return Err(invalid_input_failure());
        }
        seen |= bit;
        staged[usize::from(sample.seat.get())].merge(sample);
    }
    *inputs = staged;
    Ok(())
}

fn service_due_inputs<E>(
    protocol: &mut LiveBrowserProtocol<E>,
    inputs: &mut [BrowserSeatInput; MAX_SEATS],
    pending: &mut Option<(SimTick, SimTick)>,
    metrics: &mut BrowserOnlineClientMetrics,
    generation: u64,
) -> Result<bool, BrowserClientFailure>
where
    E: NonBlockingDatagramEndpoint,
{
    if pending.is_none()
        && let Some(due) = protocol.take_due_input_ticks().map_err(|error| {
            map_protocol_error(
                error,
                generation,
                protocol.predicted_client().confirmed_tick(),
            )
        })?
    {
        *pending = Some((due.first, due.last));
    }
    let mut submitted_any = false;
    while let Some((tick, last)) = *pending {
        let manifest = protocol.manifest().ok_or_else(synchronization_failure)?;
        let peer_id = protocol.peer_id();
        let mut staged_inputs = *inputs;
        let mut frames = [InputFrame::default(); MAX_SEATS];
        let mut count = 0;
        for assignment in manifest.ownership.as_slice() {
            if assignment.owner != SeatOwner::Peer(peer_id) {
                continue;
            }
            frames[count] = staged_inputs[usize::from(assignment.seat.get())]
                .frame_for_tick(tick, assignment.seat);
            count += 1;
        }
        match protocol.submit_local_inputs(&frames[..count]) {
            Ok(_) => {
                *inputs = staged_inputs;
                submitted_any = true;
                metrics.input_ticks_submitted = metrics.input_ticks_submitted.saturating_add(1);
                *pending = if tick < last {
                    Some((tick.next(), last))
                } else {
                    None
                };
            }
            Err(ClientProtocolError::Recoverable(
                ClientProtocolRecoverableError::OutboundBackpressure,
            )) => {
                metrics.input_backpressure_retries =
                    metrics.input_backpressure_retries.saturating_add(1);
                break;
            }
            Err(error) => {
                return Err(map_protocol_error(
                    error,
                    generation,
                    protocol.predicted_client().confirmed_tick(),
                ));
            }
        }
    }
    Ok(submitted_any)
}

fn make_status<E>(
    generation: u64,
    mode: BrowserBootstrapMode,
    protocol: &LiveBrowserProtocol<E>,
    network_tick: SimTick,
    quality: NetworkQualitySnapshot,
    worker: RemoteOnlineWorkerMetrics,
) -> RemoteOnlineClientStatus
where
    E: NonBlockingDatagramEndpoint,
{
    let phase = if matches!(mode, BrowserBootstrapMode::Reconnect { .. })
        && !matches!(
            protocol.phase(),
            ConnectionPhase::Fighting
                | ConnectionPhase::ConfirmingResult
                | ConnectionPhase::Results
        ) {
        RemoteOnlineClientPhase::Reconnecting
    } else {
        match protocol.phase() {
            ConnectionPhase::OfflineMenu
            | ConnectionPhase::Lobby
            | ConnectionPhase::Connecting
            | ConnectionPhase::Authenticating
            | ConnectionPhase::ManifestAgreement => RemoteOnlineClientPhase::Connecting,
            ConnectionPhase::Loading => RemoteOnlineClientPhase::Loading,
            ConnectionPhase::InitialSync => RemoteOnlineClientPhase::Synchronizing,
            ConnectionPhase::Ready => RemoteOnlineClientPhase::Ready,
            ConnectionPhase::Countdown => RemoteOnlineClientPhase::Countdown,
            ConnectionPhase::Fighting => RemoteOnlineClientPhase::Fighting,
            ConnectionPhase::ConfirmingResult => RemoteOnlineClientPhase::ConfirmingResult,
            ConnectionPhase::Results => RemoteOnlineClientPhase::Results,
        }
    };
    RemoteOnlineClientStatus {
        generation,
        phase,
        network_tick,
        predicted_tick: protocol.predicted_client().predicted_tick(),
        confirmed_tick: protocol.predicted_client().confirmed_tick(),
        countdown_start_tick: protocol.countdown_start_tick(),
        quality,
        protocol: protocol.metrics(),
        rollback: protocol
            .predicted_client()
            .prediction()
            .map_or_else(RollbackMetrics::default, |prediction| prediction.metrics()),
        last_hard_resync_reason: protocol.predicted_client().last_hard_resync_reason(),
        runtime: *protocol.runtime_metrics(),
        worker,
        failure: None,
        authority_disconnect: None,
    }
}

fn is_post_result_transport_retirement(error: &ClientProtocolError<LiveSimulationError>) -> bool {
    matches!(
        error,
        ClientProtocolError::Fatal(ClientProtocolFatalError::Transport(
            RuntimeConnectionState::RemoteDisconnect
                | RuntimeConnectionState::TransportDisconnected
                | RuntimeConnectionState::RetryExhausted
        ))
    )
}

fn map_protocol_error(
    error: ClientProtocolError<LiveSimulationError>,
    generation: u64,
    local_confirmed_tick: Option<SimTick>,
) -> BrowserClientFailure {
    match error {
        ClientProtocolError::Recoverable(ClientProtocolRecoverableError::OutboundBackpressure) => {
            capacity_failure().into()
        }
        ClientProtocolError::Recoverable(_) => synchronization_failure().into(),
        ClientProtocolError::Fatal(ClientProtocolFatalError::AuthorityDisconnect(message)) => {
            BrowserClientFailure::AuthorityDisconnect(RemoteAuthorityDisconnect {
                generation,
                message,
                local_confirmed_tick,
            })
        }
        ClientProtocolError::Fatal(error) => map_protocol_fatal(error).into(),
    }
}

fn map_protocol_fatal(error: ClientProtocolFatalError<LiveSimulationError>) -> OnlineFailure {
    match error {
        ClientProtocolFatalError::Session(error) => OnlineFailure::from_session(error),
        ClientProtocolFatalError::Transport(RuntimeConnectionState::RemoteDisconnect) => {
            authority_lost_failure()
        }
        ClientProtocolFatalError::AuthorityDisconnect(message) => {
            OnlineFailure::from_disconnect(message)
        }
        ClientProtocolFatalError::Transport(
            RuntimeConnectionState::TransportDisconnected | RuntimeConnectionState::RetryExhausted,
        ) => connection_failure(),
        ClientProtocolFatalError::Protocol(_)
        | ClientProtocolFatalError::UnexpectedMessage(_)
        | ClientProtocolFatalError::ConflictingStateAtTick { .. } => synchronization_failure(),
        ClientProtocolFatalError::AuthorityAbuseThreshold => malformed_authority_failure(),
        ClientProtocolFatalError::RuntimeQueue(_) => capacity_failure(),
        ClientProtocolFatalError::Resync(_)
        | ClientProtocolFatalError::Prediction(_)
        | ClientProtocolFatalError::SnapshotContractMismatch(_)
        | ClientProtocolFatalError::SnapshotApplicationMismatch { .. }
        | ClientProtocolFatalError::ConflictingResult { .. }
        | ClientProtocolFatalError::FinalStateHashMismatch { .. } => synchronization_failure(),
        ClientProtocolFatalError::SessionClock(_)
        | ClientProtocolFatalError::ClockRegressed { .. }
        | ClientProtocolFatalError::MonotonicClockRegressed { .. }
        | ClientProtocolFatalError::TimelineExhausted => clock_failure(),
        ClientProtocolFatalError::AlreadyFailed(fault) => failure_for_fault(fault),
        ClientProtocolFatalError::Transport(RuntimeConnectionState::Active) => internal_failure(),
    }
}

fn failure_for_fault(fault: ClientProtocolFault) -> OnlineFailure {
    match fault {
        ClientProtocolFault::Protocol
        | ClientProtocolFault::Resync
        | ClientProtocolFault::Prediction
        | ClientProtocolFault::SnapshotContract
        | ClientProtocolFault::Result
        | ClientProtocolFault::UnexpectedMessage
        | ClientProtocolFault::Session => synchronization_failure(),
        ClientProtocolFault::Runtime => connection_failure(),
        ClientProtocolFault::Clock => clock_failure(),
    }
}

const fn failure(
    code: OnlineFailureCode,
    severity: OnlineFailureSeverity,
    recovery: OnlineRecoveryAction,
    detail_code: u16,
) -> OnlineFailure {
    OnlineFailure {
        code,
        severity,
        recovery,
        detail_code,
    }
}

const fn internal_failure() -> OnlineFailure {
    failure(
        OnlineFailureCode::InternalFailure,
        OnlineFailureSeverity::Fatal,
        OnlineRecoveryAction::ReturnToMenu,
        100,
    )
}

const fn capacity_failure() -> OnlineFailure {
    failure(
        OnlineFailureCode::InternalCapacity,
        OnlineFailureSeverity::Fatal,
        OnlineRecoveryAction::ReturnToMenu,
        101,
    )
}

const fn incompatible_failure() -> OnlineFailure {
    failure(
        OnlineFailureCode::IncompatibleVersion,
        OnlineFailureSeverity::Fatal,
        OnlineRecoveryAction::ReturnToMenu,
        102,
    )
}

const fn synchronization_failure() -> OnlineFailure {
    failure(
        OnlineFailureCode::SynchronizationFailed,
        OnlineFailureSeverity::Fatal,
        OnlineRecoveryAction::ReturnToLobby,
        103,
    )
}

const fn clock_failure() -> OnlineFailure {
    failure(
        OnlineFailureCode::ClockSynchronizationFailed,
        OnlineFailureSeverity::Recoverable,
        OnlineRecoveryAction::Retry,
        104,
    )
}

const fn invalid_input_failure() -> OnlineFailure {
    failure(
        OnlineFailureCode::InvalidInput,
        OnlineFailureSeverity::Fatal,
        OnlineRecoveryAction::ReturnToLobby,
        105,
    )
}

const fn connection_failure() -> OnlineFailure {
    failure(
        OnlineFailureCode::ConnectionTimedOut,
        OnlineFailureSeverity::Recoverable,
        OnlineRecoveryAction::Reconnect,
        106,
    )
}

const fn authority_lost_failure() -> OnlineFailure {
    failure(
        OnlineFailureCode::AuthorityLost,
        OnlineFailureSeverity::Recoverable,
        OnlineRecoveryAction::Reconnect,
        107,
    )
}

const fn malformed_authority_failure() -> OnlineFailure {
    failure(
        OnlineFailureCode::MalformedTraffic,
        OnlineFailureSeverity::Fatal,
        OnlineRecoveryAction::ReturnToLobby,
        108,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::authority::AuthorityTickReport;
    use crate::authority_input::AuthorityInputConfig;
    use crate::authority_peer_hub::{
        AuthorityAdvanceOutcome, AuthorityPeerHub, AuthorityPeerHubConfig,
    };
    use crate::game_state::{LocalSetup, MatchPhase, MatchState};
    use crate::headless::build_headless_simulation;
    use crate::match_config::{MatchBuildOptions, build_headless_match_config};
    use crate::network_io::InProcessEndpoint;
    use crate::network_protocol::{
        AuthorityKind, DisconnectCode, MatchId, ReconnectClaim, RetryDisposition, StateHash,
    };
    use crate::reconnect::{AuthenticatedPeer, AuthenticatedUserId};

    fn peer() -> PeerId {
        PeerId::new(51).unwrap()
    }

    fn user() -> AuthenticatedUserId {
        AuthenticatedUserId::new(5_100).unwrap()
    }

    fn match_config() -> HeadlessMatchConfig {
        let setup = LocalSetup::default();
        build_headless_match_config(
            &setup,
            MatchBuildOptions::single_peer(
                MatchId::new(*b"browser-client01").unwrap(),
                AuthorityKind::Dedicated,
                false,
                peer(),
                &setup,
                SimTick(5),
            ),
        )
        .unwrap()
    }

    fn neutral_disconnected(_peer: PeerId, seat: SeatId, tick: SimTick) -> InputFrame {
        InputFrame {
            tick,
            seat,
            sequence: InputSequence(tick.get() as u16),
            ..InputFrame::default()
        }
    }

    type TestHub = AuthorityPeerHub<LiveSimulationDriver, InProcessEndpoint>;

    struct Harness {
        config: HeadlessMatchConfig,
        client: BrowserOnlineClient<InProcessEndpoint>,
        hub: TestHub,
        network_tick: SimTick,
        monotonic_micros: u64,
    }

    impl Harness {
        fn new() -> Self {
            let config = match_config();
            let simulation = build_headless_simulation(config.clone()).unwrap();
            let mut hub_config = AuthorityPeerHubConfig {
                countdown_lead_ticks: 2,
                ..AuthorityPeerHubConfig::default()
            };
            hub_config.runtime.outbound_capacity = 64;
            hub_config.runtime.inbound_capacity = 64;
            let mut hub = AuthorityPeerHub::new(
                config.manifest,
                simulation,
                AuthorityInputConfig::default(),
                &[AuthenticatedPeer {
                    peer_id: peer(),
                    user_id: user(),
                }],
                hub_config,
            )
            .unwrap();
            let (client_endpoint, authority_endpoint) = InProcessEndpoint::pair(512).unwrap();
            hub.attach_initial(peer(), user(), authority_endpoint)
                .unwrap();
            let mut client = BrowserOnlineClient::new(
                client_endpoint,
                config.clone(),
                peer(),
                BrowserOnlineClientConfig::default(),
            )
            .unwrap();
            client.mark_content_loaded();
            Self {
                config,
                client,
                hub,
                network_tick: SimTick::ZERO,
                monotonic_micros: 0,
            }
        }

        fn round(&mut self, advance_authority: bool) -> Option<AuthorityTickReport> {
            let report = self.client.service(self.monotonic_micros);
            assert_eq!(report.fixed_steps, 1, "test clock services one fixed tick");
            assert_eq!(report.pending_fixed_ticks, 0);
            self.network_tick = self.network_tick.next();
            self.hub.pump_network(self.network_tick).unwrap();
            let authority_report = if advance_authority {
                match self.hub.try_advance(neutral_disconnected).unwrap() {
                    (AuthorityAdvanceOutcome::Advanced, report) => report,
                    (_, None) => None,
                    (outcome, report) => {
                        panic!("invalid authority outcome {outcome:?}: {report:?}")
                    }
                }
            } else {
                None
            };
            self.hub.pump_network(self.network_tick).unwrap();
            self.monotonic_micros = self.monotonic_micros.saturating_add(
                MICROS_PER_SECOND.div_ceil(u64::from(AUTHORITY_THREAD_TICK_RATE_HZ)),
            );
            authority_report
        }

        fn drive_until_fighting(&mut self) {
            for _ in 0..512 {
                self.round(true);
                if self.client.status().phase == RemoteOnlineClientPhase::Fighting {
                    return;
                }
            }
            panic!(
                "browser startup did not reach fighting: {:?}",
                self.client.status()
            );
        }

        fn settle(&mut self, rounds: usize) {
            for _ in 0..rounds {
                if self.client.terminal().is_some() {
                    break;
                }
                self.round(true);
            }
        }
    }

    #[test]
    fn fixed_clock_bounds_catch_up_without_dropping_ticks() {
        let mut clock = BrowserFixedTickClock::default();
        assert_eq!(clock.observe(10_000).unwrap(), 1);
        clock.commit_tick();
        assert_eq!(clock.observe(1_010_000).unwrap(), 60);
        for _ in 0..8 {
            let now = clock.next_time().unwrap();
            assert_eq!(now.network_tick, SimTick(clock.serviced_ticks + 1));
            clock.commit_tick();
        }
        assert_eq!(clock.observe(1_010_000).unwrap(), 52);
        assert!(clock.observe(9_999).is_err());
    }

    #[test]
    fn staged_input_keeps_edges_until_a_protocol_tick_is_accepted() {
        let seat = SeatId::new(0).unwrap();
        let mut input = BrowserSeatInput::default();
        input.merge(RemoteLocalInputSample {
            seat,
            held_buttons: InputButtons::new(InputButtons::LIGHT).unwrap(),
            pressed_buttons: InputButtons::new(InputButtons::LIGHT).unwrap(),
            ..RemoteLocalInputSample::default()
        });

        let mut rejected_stage = input;
        let rejected = rejected_stage.frame_for_tick(SimTick(7), seat);
        assert_eq!(rejected.pressed_buttons.bits(), InputButtons::LIGHT);

        let accepted = input.frame_for_tick(SimTick(7), seat);
        assert_eq!(accepted.pressed_buttons.bits(), InputButtons::LIGHT);
        let following = input.frame_for_tick(SimTick(8), seat);
        assert_eq!(following.held_buttons.bits(), InputButtons::LIGHT);
        assert_eq!(following.pressed_buttons.bits(), 0);
        assert_eq!(following.sequence.0, accepted.sequence.0.wrapping_add(1));
    }

    #[test]
    fn production_protocol_reaches_fighting_and_projects_a_separate_world() {
        let mut harness = Harness::new();
        harness.drive_until_fighting();
        assert!(harness.client.status().countdown_start_tick.is_some());
        assert!(harness.client.metrics().fixed_steps > 0);

        let batch = RemoteLocalInputBatch::new(&[RemoteLocalInputSample {
            seat: SeatId::new(0).unwrap(),
            movement_x: crate::network_protocol::QuantizedAxis::new(100).unwrap(),
            held_buttons: InputButtons::new(InputButtons::LIGHT).unwrap(),
            pressed_buttons: InputButtons::new(InputButtons::LIGHT).unwrap(),
            ..RemoteLocalInputSample::default()
        }])
        .unwrap();
        harness.client.submit_inputs(batch).unwrap();
        for _ in 0..12 {
            harness.round(true);
        }

        let mut render_world = build_headless_simulation(harness.config.clone()).unwrap();
        let update = harness
            .client
            .project_latest(render_world.world_mut())
            .unwrap();
        let projection = update
            .projection
            .expect("browser prediction published a snapshot");
        assert_eq!(projection.snapshot_tick, render_world.current_sim_tick());
        assert_eq!(
            render_world.current_sim_tick(),
            harness.client.status().predicted_tick.unwrap()
        );
        assert_ne!(
            render_world.world() as *const World,
            harness.hub.authority().simulation().world() as *const World
        );
    }

    #[test]
    fn confirmed_result_and_exact_final_projection_are_retained() {
        let mut harness = Harness::new();
        harness.drive_until_fighting();
        harness
            .hub
            .authority_mut()
            .simulation_mut()
            .world_mut()
            .resource_mut::<MatchState>()
            .phase = MatchPhase::Results;
        for _ in 0..64 {
            harness.round(true);
            if harness.client.terminal().is_some() {
                break;
            }
        }
        let result = harness
            .client
            .confirmed_result()
            .expect("browser retained the verified result");
        assert_eq!(
            harness.client.terminal(),
            Some(RemoteOnlineTerminal::Completed(result))
        );

        let mut render_world = build_headless_simulation(harness.config.clone()).unwrap();
        let update = harness
            .client
            .project_latest(render_world.world_mut())
            .unwrap();
        assert_eq!(update.projected_confirmed_result, Some(result));
        assert_eq!(render_world.state_hash().unwrap(), result.final_hash.0);
        assert_eq!(
            render_world
                .world()
                .resource::<ConfirmedMatchPresentation>()
                .final_tick,
            result.final_tick
        );
        assert_eq!(
            render_world
                .world()
                .resource::<ConfirmedProgressionLedger>()
                .len(),
            1
        );
        harness.client.stop();
        assert_eq!(
            harness.client.terminal(),
            Some(RemoteOnlineTerminal::Completed(result))
        );
        assert_eq!(
            result.final_hash,
            StateHash(render_world.state_hash().unwrap())
        );
    }

    #[test]
    fn reconnect_reuses_authority_countdown_boundary_and_fresh_endpoint() {
        let mut harness = Harness::new();
        harness.drive_until_fighting();
        harness.settle(5);
        let before = harness.client.status();
        let old_connection = harness.hub.connection_for_peer(peer()).unwrap();
        harness.hub.detach(old_connection).unwrap();

        let (client_endpoint, authority_endpoint) = InProcessEndpoint::pair(512).unwrap();
        harness
            .hub
            .attach_reconnect(
                user(),
                ReconnectClaim {
                    match_id: harness.config.manifest.match_id,
                    peer_id: peer(),
                    last_confirmed_tick: before.confirmed_tick.unwrap_or(SimTick::ZERO),
                },
                authority_endpoint,
            )
            .unwrap();
        harness.client.reconnect(client_endpoint).unwrap();
        assert_eq!(harness.client.generation(), 2);
        assert_eq!(
            harness.client.status().phase,
            RemoteOnlineClientPhase::Reconnecting
        );

        for _ in 0..512 {
            harness.round(true);
            if harness.client.status().phase == RemoteOnlineClientPhase::Fighting {
                break;
            }
        }
        let after = harness.client.status();
        assert_eq!(after.phase, RemoteOnlineClientPhase::Fighting);
        assert_eq!(after.countdown_start_tick, before.countdown_start_tick);
        assert!(after.confirmed_tick >= before.confirmed_tick);
        assert_eq!(harness.hub.metrics().reconnects_completed, 1);
    }

    #[test]
    fn typed_authority_disconnect_retains_exact_payload_and_progress() {
        let mut harness = Harness::new();
        harness.drive_until_fighting();
        harness.settle(4);
        let before = harness.client.status();
        let connection = harness.hub.connection_for_peer(peer()).unwrap();
        harness.hub.revoke_authentication(connection).unwrap();

        for _ in 0..16 {
            harness.network_tick = harness.network_tick.next();
            harness.hub.pump_network(harness.network_tick).unwrap();
            let report = harness.client.service(harness.monotonic_micros);
            harness.monotonic_micros = harness.monotonic_micros.saturating_add(
                MICROS_PER_SECOND.div_ceil(u64::from(AUTHORITY_THREAD_TICK_RATE_HZ)),
            );
            assert!(report.fixed_steps <= 1);
            if harness.client.terminal().is_some() {
                break;
            }
        }
        let Some(RemoteOnlineTerminal::AuthorityDisconnected(disconnect)) =
            harness.client.terminal()
        else {
            panic!(
                "typed authority disconnect was not retained: {:?}",
                harness.client.status()
            );
        };
        assert_eq!(disconnect.generation, 1);
        assert_eq!(disconnect.local_confirmed_tick, before.confirmed_tick);
        assert_eq!(
            disconnect.message.code,
            DisconnectCode::AuthenticationFailed
        );
        assert_eq!(disconnect.message.retry, RetryDisposition::Fatal);
        assert_eq!(disconnect.message.detail_code, 13);
        assert_eq!(
            harness.client.status().authority_disconnect,
            Some(disconnect)
        );
    }

    #[test]
    fn regressed_browser_clock_fails_closed() {
        let mut harness = Harness::new();
        let _ = harness.client.service(10_000);
        let report = harness.client.service(9_999);
        let Some(RemoteOnlineTerminal::Failed(failure)) = report.terminal else {
            panic!("clock regression was not terminal: {report:?}");
        };
        assert_eq!(failure.code, OnlineFailureCode::ClockSynchronizationFailed);
        assert_eq!(report.status.phase, RemoteOnlineClientPhase::Failed);
    }
}
