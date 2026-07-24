//! Single-sided production protocol orchestration for one predicted client.
//!
//! Unlike [`crate::local_loopback::LocalLoopbackMatch`], this driver never owns
//! or advances an authority. It owns exactly one client [`NetworkRuntime`] and
//! one [`PredictedClient`], and can therefore be connected to UDP, Steam, or an
//! in-process authority without changing prediction/session semantics.

use core::fmt;

use crate::headless::snapshot_contract_for_manifest;
use crate::local_loopback::{
    AppliedCanonicalSnapshot, ClientAuthorityOutcome, InitialSnapshotTarget,
};
use crate::network_codec::{
    Handshake, ResultIdentifier, StateDeltaAndAcks, StateHashAndAcks, WireMessage,
};
use crate::network_io::NonBlockingDatagramEndpoint;
use crate::network_protocol::{
    ClockProbe, ClockProbeId, ClockReply, CommittedInputRelay, CommittedInputSource,
    ConnectionPhase, DisconnectMessage, InputBatch, InputFrame, MAX_INPUT_FRAMES_PER_WINDOW,
    MAX_SEATS, MatchId, MatchManifest, PeerId, ProtocolValidationError, ResyncInputTail,
    ResyncReason, SeatId, SeatInputWindow, SeatOwner, SimTick, StartMessage, StateHash,
};
use crate::network_runtime::{
    NetworkRuntime, PumpReport, QueueDisposition, RuntimeAbuseSignal, RuntimeConfig,
    RuntimeConfigError, RuntimeConnectionState, RuntimeEvent, RuntimeMetrics, RuntimeQueueError,
};
use crate::predicted_client::{PredictedClient, PredictedClientError, PredictedClientMetrics};
use crate::resync_transfer::{
    ClientResyncAssembler, DEFAULT_RESYNC_TIMEOUT_TICKS, ResyncBeginOutcome, ResyncChunkOutcome,
    ResyncInputTailOutcome, ResyncTransferError, ResyncTransferMetrics,
};
use crate::rollback::{
    NoopEventDiscard, NoopRollbackTiming, RollbackEventDiscard, RollbackTimingHook, RollbackWorld,
};
use crate::session::{
    AppliedInitialSync, ClientSession, ConfirmedSessionResult, SessionError, SessionTimeouts,
};
use crate::session_clock::{
    AuthorityClockSynchronizer, AuthorityTickEstimate, ClockRoundTripSample, DueInputTicks,
    InputLeadScheduler, SessionClockError, SessionClockMetrics,
};
use crate::snapshot::CanonicalSnapshot;

pub const DEFAULT_CLOCK_PROBE_INTERVAL_MICROS: u64 = 1_000_000;
const MAX_RECENT_APPLIED_RESYNC_TAILS: usize = 4;
/// Reliable result identity may overtake its final unreliable input/state
/// packets. Give those independent channels one rollback window to arrive,
/// then close the match through the existing reliable snapshot+input-tail path.
pub const RESULT_REPAIR_GRACE_TICKS: u64 = 12;

/// The driver performs a fixed amount of transport work per call through
/// [`RuntimeConfig`]. Snapshot assembly is additionally capped by protocol
/// constants in [`ClientResyncAssembler`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClientProtocolConfig {
    pub runtime: RuntimeConfig,
    pub session_timeouts: SessionTimeouts,
    pub resync_timeout_ticks: u64,
    /// Refresh cadence after initial synchronization. Before readiness the
    /// single outstanding probe is replenished immediately after every reply.
    pub clock_probe_interval_micros: u64,
}

impl Default for ClientProtocolConfig {
    fn default() -> Self {
        Self {
            runtime: RuntimeConfig::default(),
            session_timeouts: SessionTimeouts::default(),
            resync_timeout_ticks: DEFAULT_RESYNC_TIMEOUT_TICKS,
            clock_probe_interval_micros: DEFAULT_CLOCK_PROBE_INTERVAL_MICROS,
        }
    }
}

impl ClientProtocolConfig {
    pub fn validate(self) -> Result<(), ClientProtocolBuildError> {
        self.runtime
            .validate()
            .map_err(ClientProtocolBuildError::RuntimeConfig)?;
        if !self.session_timeouts.validate() {
            return Err(ClientProtocolBuildError::Session(
                SessionError::InvalidTimeoutPolicy,
            ));
        }
        if self.resync_timeout_ticks == 0 {
            return Err(ClientProtocolBuildError::InvalidResyncTimeout);
        }
        if self.clock_probe_interval_micros == 0 {
            return Err(ClientProtocolBuildError::InvalidClockProbeInterval);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientProtocolBuildError {
    Protocol(ProtocolValidationError),
    Session(SessionError),
    RuntimeConfig(RuntimeConfigError),
    RuntimeQueue(RuntimeQueueError),
    InvalidResyncTimeout,
    InvalidClockProbeInterval,
}

impl fmt::Display for ClientProtocolBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "remote client protocol bootstrap failed: {self:?}"
        )
    }
}

impl std::error::Error for ClientProtocolBuildError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientProtocolFault {
    Protocol,
    Session,
    Runtime,
    Resync,
    Prediction,
    SnapshotContract,
    Result,
    Clock,
    UnexpectedMessage,
}

/// Conditions for which the caller may safely retry without rebuilding the
/// connection or predicted world.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientProtocolRecoverableError {
    OutboundBackpressure,
    ResyncInFlight {
        reason: ResyncReason,
    },
    NotFighting {
        phase: ConnectionPhase,
    },
    PredictionNotInitialized,
    InputNotDue {
        next_tick: SimTick,
        scheduled_through: SimTick,
    },
}

/// Fail-closed protocol errors. Once one is returned, this driver latches its
/// coarse [`ClientProtocolFault`] and rejects further mutation.
#[derive(Debug)]
pub enum ClientProtocolFatalError<WorldError> {
    AlreadyFailed(ClientProtocolFault),
    Protocol(ProtocolValidationError),
    Session(SessionError),
    RuntimeQueue(RuntimeQueueError),
    Resync(ResyncTransferError),
    SessionClock(SessionClockError),
    Prediction(PredictedClientError<WorldError>),
    Transport(RuntimeConnectionState),
    /// Authenticated authority-authored match termination. The application
    /// retains the complete bounded payload so its retry disposition cannot be
    /// collapsed into a generic socket-close reason.
    AuthorityDisconnect(DisconnectMessage),
    AuthorityAbuseThreshold,
    ClockRegressed {
        previous: SimTick,
        received: SimTick,
    },
    MonotonicClockRegressed {
        previous_micros: u64,
        received_micros: u64,
    },
    UnexpectedMessage(&'static str),
    SnapshotContractMismatch(&'static str),
    SnapshotApplicationMismatch {
        expected: AppliedCanonicalSnapshot,
        actual: AppliedCanonicalSnapshot,
    },
    ConflictingStateAtTick {
        tick: SimTick,
        first: StateHash,
        second: StateHash,
    },
    ConflictingResult {
        first: u64,
        second: u64,
    },
    FinalStateHashMismatch {
        tick: SimTick,
        result: StateHash,
        confirmed: StateHash,
    },
    TimelineExhausted,
}

impl<WorldError> ClientProtocolFatalError<WorldError> {
    const fn fault(&self) -> ClientProtocolFault {
        match self {
            Self::AlreadyFailed(fault) => *fault,
            Self::Protocol(_) | Self::ConflictingStateAtTick { .. } => {
                ClientProtocolFault::Protocol
            }
            Self::Session(_) => ClientProtocolFault::Session,
            Self::RuntimeQueue(_)
            | Self::Transport(_)
            | Self::AuthorityDisconnect(_)
            | Self::AuthorityAbuseThreshold => ClientProtocolFault::Runtime,
            Self::Resync(_) => ClientProtocolFault::Resync,
            Self::Prediction(_) => ClientProtocolFault::Prediction,
            Self::SnapshotContractMismatch(_) | Self::SnapshotApplicationMismatch { .. } => {
                ClientProtocolFault::SnapshotContract
            }
            Self::ConflictingResult { .. } | Self::FinalStateHashMismatch { .. } => {
                ClientProtocolFault::Result
            }
            Self::SessionClock(_)
            | Self::ClockRegressed { .. }
            | Self::MonotonicClockRegressed { .. }
            | Self::TimelineExhausted => ClientProtocolFault::Clock,
            Self::UnexpectedMessage(_) => ClientProtocolFault::UnexpectedMessage,
        }
    }
}

impl<WorldError: fmt::Debug> fmt::Display for ClientProtocolFatalError<WorldError> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "remote client protocol failed: {self:?}")
    }
}

impl<WorldError: fmt::Debug> std::error::Error for ClientProtocolFatalError<WorldError> {}

#[derive(Debug)]
pub enum ClientProtocolError<WorldError> {
    Recoverable(ClientProtocolRecoverableError),
    Fatal(ClientProtocolFatalError<WorldError>),
}

impl<WorldError> ClientProtocolError<WorldError> {
    pub const fn is_recoverable(&self) -> bool {
        matches!(self, Self::Recoverable(_))
    }
}

impl<WorldError: fmt::Debug> fmt::Display for ClientProtocolError<WorldError> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Recoverable(error) => {
                write!(formatter, "recoverable client condition: {error:?}")
            }
            Self::Fatal(error) => error.fmt(formatter),
        }
    }
}

impl<WorldError: fmt::Debug> std::error::Error for ClientProtocolError<WorldError> {}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClientProtocolMetrics {
    pub network_pumps: u64,
    pub phase_transitions: u64,
    pub manifests_received: u64,
    pub content_loads_completed: u64,
    pub initial_snapshots_applied: u64,
    pub hard_resync_snapshots_applied: u64,
    pub resync_requests: u64,
    pub hard_resync_requests: u64,
    /// Previously transmitted local future ticks replayed after a repair
    /// snapshot so prediction and input sequencing retain one generation.
    pub local_input_ticks_replayed_after_resync: u64,
    pub resync_transfer_timeouts: u64,
    pub resync_transfer_supersessions: u64,
    pub late_duplicate_resync_input_tails: u64,
    pub reconnect_replication_deferred: u64,
    pub clock_probes_queued: u64,
    pub clock_replies_accepted: u64,
    pub clock_synchronized_transitions: u64,
    pub periodic_clock_refreshes: u64,
    pub local_input_batches: u64,
    pub local_input_frames: u64,
    pub maximum_local_seats_in_batch: u8,
    pub maximum_input_redundancy: u8,
    pub committed_relays: u64,
    pub state_hash_messages: u64,
    pub state_delta_messages: u64,
    pub stale_state_messages: u64,
    pub matched_authority_states: u64,
    pub rollback_corrections: u64,
    pub results_received: u64,
    pub results_deferred: u64,
    pub result_wait_pumps: u64,
    pub result_repair_requests: u64,
    pub confirmed_results: u64,
    pub outbound_backpressure: u64,
    pub recoverable_errors: u64,
    pub fatal_errors: u64,
}

/// The transport/session retry clock and local monotonic timestamp are separate
/// on purpose. Integer microseconds are used only by the non-canonical clock
/// estimator; canonical gameplay still advances exclusively in `SimTick`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClientProtocolTime {
    pub network_tick: SimTick,
    pub monotonic_micros: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClientProtocolPumpReport {
    pub runtime: PumpReport,
    pub phase_before: ConnectionPhase,
    pub phase_after: ConnectionPhase,
    pub authority_tick_estimate: Option<AuthorityTickEstimate>,
    pub result_waiting_for_final_state: bool,
    pub resync_in_flight: Option<ResyncReason>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalInputSubmitReport {
    pub tick: SimTick,
    pub seats: u8,
    pub queue: QueueDisposition,
}

#[derive(Clone, Copy, Debug, Default)]
struct LocalSeatInputHistory {
    frames: [InputFrame; MAX_INPUT_FRAMES_PER_WINDOW],
    len: u8,
}

impl LocalSeatInputHistory {
    fn newest(&self) -> Option<InputFrame> {
        (self.len != 0).then_some(self.frames[0])
    }

    fn frame_at(&self, tick: SimTick) -> Option<InputFrame> {
        self.frames[..usize::from(self.len)]
            .iter()
            .find(|frame| frame.tick == tick)
            .copied()
    }

    fn push(&mut self, frame: InputFrame) -> Result<(), (SimTick, SimTick)> {
        if self.len != 0 {
            let previous = self.frames[0];
            if frame.tick == previous.tick {
                return Err((previous.tick, frame.tick));
            }
            if frame.tick != previous.tick.next()
                || frame.sequence.0 != previous.sequence.0.wrapping_add(1)
            {
                // A correction or reconnect can jump the canonical frontier.
                // Start a new redundancy tail; never synthesize missing frames.
                self.frames = [InputFrame::default(); MAX_INPUT_FRAMES_PER_WINDOW];
                self.frames[0] = frame;
                self.len = 1;
                return Ok(());
            }
        }
        let retained = usize::from(self.len).min(MAX_INPUT_FRAMES_PER_WINDOW - 1);
        self.frames.copy_within(0..retained, 1);
        self.frames[0] = frame;
        self.len = (retained + 1) as u8;
        Ok(())
    }

    fn window(&self) -> Result<SeatInputWindow, ProtocolValidationError> {
        SeatInputWindow::from_newest_first(&self.frames[..usize::from(self.len)])
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClientPostSyncStage {
    Applied,
    InitialSync,
    Ready,
}

#[derive(Clone, Copy, Debug)]
struct PendingClientPostSync {
    applied: crate::network_protocol::ResyncApplied,
    stage: ClientPostSyncStage,
    initial: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingClockProbe {
    probe_id: ClockProbeId,
    sent_micros: u64,
}

/// A remote client protocol endpoint. This type has no authority field, no
/// authority clock, and no way to construct a transport peer.
pub struct RemotePredictedClientProtocol<E, W, D = NoopEventDiscard, T = NoopRollbackTiming>
where
    E: NonBlockingDatagramEndpoint,
    W: RollbackWorld<Snapshot = CanonicalSnapshot>,
    D: RollbackEventDiscard,
    T: RollbackTimingHook,
{
    expected_match_id: MatchId,
    expected_compatibility: crate::network_protocol::CompatibilityId,
    peer_id: PeerId,
    runtime: NetworkRuntime<E>,
    predicted: PredictedClient<W, D, T>,
    config: ClientProtocolConfig,
    manifest: Option<MatchManifest>,
    assembler: Option<ClientResyncAssembler>,
    authority_clock: AuthorityClockSynchronizer,
    input_scheduler: Option<InputLeadScheduler>,
    pending_clock_probe: Option<PendingClockProbe>,
    next_clock_probe_id: u32,
    last_clock_probe_sent_micros: Option<u64>,
    input_histories: [LocalSeatInputHistory; MAX_SEATS],
    pending_resync_request: Option<crate::network_protocol::ResyncRequest>,
    resync_in_flight: Option<ResyncReason>,
    pending_post_sync: Option<PendingClientPostSync>,
    pending_reconnect_resume: Option<AppliedInitialSync>,
    recent_applied_input_tails: [Option<ResyncInputTail>; MAX_RECENT_APPLIED_RESYNC_TAILS],
    recent_applied_input_tail_cursor: usize,
    initial_snapshot: Option<AppliedInitialSync>,
    canonical_snapshot_applied: bool,
    pending_result: Option<ResultIdentifier>,
    pending_result_since: Option<SimTick>,
    confirmed_result: Option<ConfirmedSessionResult>,
    result_verified: bool,
    latest_state_identity: Option<(SimTick, StateHash)>,
    last_network_tick: SimTick,
    last_monotonic_micros: u64,
    failure: Option<ClientProtocolFault>,
    metrics: ClientProtocolMetrics,
}

impl<E, W, D, T> RemotePredictedClientProtocol<E, W, D, T>
where
    E: NonBlockingDatagramEndpoint,
    W: RollbackWorld<Snapshot = CanonicalSnapshot>,
    D: RollbackEventDiscard,
    T: RollbackTimingHook,
{
    pub fn new(
        endpoint: E,
        expected_match_id: MatchId,
        peer_id: PeerId,
        expected_compatibility: crate::network_protocol::CompatibilityId,
        predicted: PredictedClient<W, D, T>,
        config: ClientProtocolConfig,
        now: ClientProtocolTime,
    ) -> Result<Self, ClientProtocolBuildError> {
        config.validate()?;
        expected_match_id
            .validate()
            .map_err(ClientProtocolBuildError::Protocol)?;
        peer_id
            .validate()
            .map_err(ClientProtocolBuildError::Protocol)?;
        expected_compatibility
            .validate()
            .map_err(ClientProtocolBuildError::Protocol)?;

        // Platform authentication and lobby admission happen before this seam.
        // Their stable peer identity is then bound into the AFC client session.
        let mut session = ClientSession::new(
            expected_compatibility,
            config.session_timeouts,
            now.network_tick,
        )
        .map_err(ClientProtocolBuildError::Session)?;
        session
            .enter_lobby(now.network_tick)
            .and_then(|()| session.start_connecting(now.network_tick))
            .and_then(|()| session.transport_connected(now.network_tick))
            .and_then(|()| session.authentication_succeeded(peer_id, now.network_tick))
            .map_err(ClientProtocolBuildError::Session)?;
        let mut runtime =
            NetworkRuntime::new_client(endpoint, expected_compatibility, session, config.runtime)
                .map_err(ClientProtocolBuildError::RuntimeConfig)?;
        runtime
            .queue_message(WireMessage::Handshake(Handshake {
                compatibility: expected_compatibility,
            }))
            .map_err(ClientProtocolBuildError::RuntimeQueue)?;

        Ok(Self {
            expected_match_id,
            expected_compatibility,
            peer_id,
            runtime,
            predicted,
            config,
            manifest: None,
            assembler: None,
            authority_clock: AuthorityClockSynchronizer::default(),
            input_scheduler: None,
            pending_clock_probe: None,
            next_clock_probe_id: 1,
            last_clock_probe_sent_micros: None,
            input_histories: [LocalSeatInputHistory::default(); MAX_SEATS],
            pending_resync_request: None,
            resync_in_flight: None,
            pending_post_sync: None,
            pending_reconnect_resume: None,
            recent_applied_input_tails: [None; MAX_RECENT_APPLIED_RESYNC_TAILS],
            recent_applied_input_tail_cursor: 0,
            initial_snapshot: None,
            canonical_snapshot_applied: false,
            pending_result: None,
            pending_result_since: None,
            confirmed_result: None,
            result_verified: false,
            latest_state_identity: None,
            last_network_tick: now.network_tick,
            last_monotonic_micros: now.monotonic_micros,
            failure: None,
            metrics: ClientProtocolMetrics::default(),
        })
    }

    /// Builds a replacement client for an already-running match. The caller
    /// must retain the exact accepted manifest and authority-selected countdown
    /// boundary from the original connection. The authority then sends its
    /// reconnect transfer without another manifest/request exchange.
    pub fn new_reconnect(
        endpoint: E,
        manifest: MatchManifest,
        peer_id: PeerId,
        countdown_start_tick: SimTick,
        mut predicted: PredictedClient<W, D, T>,
        config: ClientProtocolConfig,
        now: ClientProtocolTime,
    ) -> Result<Self, ClientProtocolBuildError> {
        config.validate()?;
        manifest
            .validate()
            .map_err(ClientProtocolBuildError::Protocol)?;
        peer_id
            .validate()
            .map_err(ClientProtocolBuildError::Protocol)?;
        if !manifest.ownership.peer_owns_any_seat(peer_id) {
            return Err(ClientProtocolBuildError::Protocol(
                ProtocolValidationError::UnownedSeat,
            ));
        }
        predicted
            .configure_manifest(&manifest)
            .map_err(|error| match error {
                PredictedClientError::Protocol(error) => ClientProtocolBuildError::Protocol(error),
                _ => ClientProtocolBuildError::Protocol(ProtocolValidationError::InvalidManifest),
            })?;
        let session = ClientSession::new_reconnect(
            manifest.compatibility,
            config.session_timeouts,
            peer_id,
            manifest,
            countdown_start_tick,
            now.network_tick,
        )
        .map_err(ClientProtocolBuildError::Session)?;
        let mut runtime =
            NetworkRuntime::new_client(endpoint, manifest.compatibility, session, config.runtime)
                .map_err(ClientProtocolBuildError::RuntimeConfig)?;
        runtime
            .queue_message(WireMessage::Handshake(Handshake {
                compatibility: manifest.compatibility,
            }))
            .map_err(ClientProtocolBuildError::RuntimeQueue)?;
        let assembler =
            ClientResyncAssembler::new(manifest.match_id, peer_id, config.resync_timeout_ticks)
                .map_err(|error| match error {
                    ResyncTransferError::Protocol(error) => {
                        ClientProtocolBuildError::Protocol(error)
                    }
                    _ => ClientProtocolBuildError::InvalidResyncTimeout,
                })?;
        let mut metrics = ClientProtocolMetrics::default();
        metrics.manifests_received = 1;

        Ok(Self {
            expected_match_id: manifest.match_id,
            expected_compatibility: manifest.compatibility,
            peer_id,
            runtime,
            predicted,
            config,
            manifest: Some(manifest),
            assembler: Some(assembler),
            authority_clock: AuthorityClockSynchronizer::default(),
            input_scheduler: None,
            pending_clock_probe: None,
            next_clock_probe_id: 1,
            last_clock_probe_sent_micros: None,
            input_histories: [LocalSeatInputHistory::default(); MAX_SEATS],
            pending_resync_request: None,
            resync_in_flight: Some(ResyncReason::Reconnect),
            pending_post_sync: None,
            pending_reconnect_resume: None,
            recent_applied_input_tails: [None; MAX_RECENT_APPLIED_RESYNC_TAILS],
            recent_applied_input_tail_cursor: 0,
            initial_snapshot: None,
            canonical_snapshot_applied: false,
            pending_result: None,
            pending_result_since: None,
            confirmed_result: None,
            result_verified: false,
            latest_state_identity: None,
            last_network_tick: now.network_tick,
            last_monotonic_micros: now.monotonic_micros,
            failure: None,
            metrics,
        })
    }

    pub const fn expected_match_id(&self) -> MatchId {
        self.expected_match_id
    }

    pub const fn peer_id(&self) -> PeerId {
        self.peer_id
    }

    pub const fn manifest(&self) -> Option<&MatchManifest> {
        self.manifest.as_ref()
    }

    pub fn countdown_start_tick(&self) -> Option<SimTick> {
        self.runtime
            .client_session()
            .and_then(ClientSession::countdown_start_tick)
    }

    /// The public phase deliberately masks the runtime's packet-level `Results`
    /// transition until the predicted history contains the exact final tick/hash.
    pub fn phase(&self) -> ConnectionPhase {
        if self.pending_result.is_some() && !self.result_verified {
            ConnectionPhase::ConfirmingResult
        } else {
            self.runtime
                .client_session()
                .expect("remote client runtime always owns a client session")
                .phase()
        }
    }

    pub const fn initial_snapshot(&self) -> Option<AppliedInitialSync> {
        self.initial_snapshot
    }

    pub const fn failure(&self) -> Option<ClientProtocolFault> {
        self.failure
    }

    pub const fn metrics(&self) -> ClientProtocolMetrics {
        self.metrics
    }

    pub fn runtime_metrics(&self) -> &RuntimeMetrics {
        self.runtime.metrics()
    }

    pub fn predicted_metrics(&self) -> PredictedClientMetrics {
        self.predicted.metrics()
    }

    pub fn resync_metrics(&self) -> Option<&ResyncTransferMetrics> {
        self.assembler.as_ref().map(ClientResyncAssembler::metrics)
    }

    pub fn session_clock_metrics(&self) -> SessionClockMetrics {
        self.authority_clock.metrics()
    }

    pub const fn is_clock_synchronized(&self) -> bool {
        self.authority_clock.is_synchronized()
    }

    pub fn authority_tick_estimate(
        &self,
        monotonic_micros: u64,
    ) -> Result<Option<AuthorityTickEstimate>, SessionClockError> {
        if !self.authority_clock.is_synchronized() {
            return Ok(None);
        }
        self.authority_clock.estimate(monotonic_micros).map(Some)
    }

    pub const fn predicted_client(&self) -> &PredictedClient<W, D, T> {
        &self.predicted
    }

    /// Consumes the next bounded gameplay-input range implied by the synchronized
    /// authority clock. Callers submit every tick in ascending order, sampling
    /// all locally owned seats for each tick. During Countdown the negotiated
    /// input lead is prefilled so tick-one input can cross the network before the
    /// shared start boundary rather than pausing the canonical match clock.
    pub fn take_due_input_ticks(
        &mut self,
    ) -> Result<Option<DueInputTicks>, ClientProtocolError<W::Error>> {
        self.ensure_active()?;
        let result = self.take_due_input_ticks_inner();
        self.record_result(result)
    }

    fn take_due_input_ticks_inner(
        &mut self,
    ) -> Result<Option<DueInputTicks>, ClientProtocolError<W::Error>> {
        if self.resync_in_flight.is_some() {
            return Ok(None);
        }
        let phase = self.phase();
        if !matches!(
            phase,
            ConnectionPhase::Countdown | ConnectionPhase::Fighting
        ) {
            return Ok(None);
        }
        self.manifest.ok_or_else(|| {
            Self::fatal(ClientProtocolFatalError::UnexpectedMessage(
                "input scheduling began without a manifest",
            ))
        })?;
        let countdown_start_tick = self
            .runtime
            .client_session()
            .and_then(ClientSession::countdown_start_tick)
            .ok_or_else(|| {
                Self::fatal(ClientProtocolFatalError::UnexpectedMessage(
                    "input scheduling began without an authority countdown boundary",
                ))
            })?;
        let scheduler = self.input_scheduler.as_mut().ok_or_else(|| {
            Self::fatal(ClientProtocolFatalError::UnexpectedMessage(
                "input scheduling began before a canonical snapshot was applied",
            ))
        })?;
        if phase == ConnectionPhase::Countdown {
            return scheduler
                .due_ticks(AuthorityTickEstimate::default())
                .map_err(|error| Self::fatal(ClientProtocolFatalError::SessionClock(error)));
        }
        let Some(authority) = self
            .authority_clock
            .estimate(self.last_monotonic_micros)
            .map_err(|error| Self::fatal(ClientProtocolFatalError::SessionClock(error)))?
            .for_match(countdown_start_tick)
        else {
            return Ok(None);
        };
        scheduler
            .due_ticks(authority)
            .map_err(|error| Self::fatal(ClientProtocolFatalError::SessionClock(error)))
    }

    /// Returns the verified result once. Packet receipt alone never populates
    /// this slot; final canonical state must first be confirmed below.
    pub fn take_confirmed_result(&mut self) -> Option<ConfirmedSessionResult> {
        self.confirmed_result.take()
    }

    /// Called by the content/loading layer after the accepted manifest's assets
    /// and definitions are ready. It schedules, but does not synchronously send,
    /// the bounded initial snapshot request.
    pub fn mark_content_loaded(
        &mut self,
        now: ClientProtocolTime,
    ) -> Result<(), ClientProtocolError<W::Error>> {
        self.ensure_active()?;
        let result = self.mark_content_loaded_inner(now);
        self.record_result(result)
    }

    fn mark_content_loaded_inner(
        &mut self,
        now: ClientProtocolTime,
    ) -> Result<(), ClientProtocolError<W::Error>> {
        self.observe_clock(now)?;
        if self.manifest.is_none() || self.assembler.is_none() {
            return Err(Self::fatal(ClientProtocolFatalError::UnexpectedMessage(
                "content was marked loaded before a manifest was accepted",
            )));
        }
        self.runtime
            .client_session_mut()
            .expect("remote client runtime always owns a client session")
            .content_loaded(now.network_tick)
            .map_err(|error| Self::fatal(ClientProtocolFatalError::Session(error)))?;
        self.schedule_resync(ResyncReason::InitialSync, SimTick::ZERO, StateHash(0))?;
        self.metrics.content_loads_completed =
            self.metrics.content_loads_completed.saturating_add(1);
        Ok(())
    }

    /// Pumps only this client's endpoint and protocol state. `network_tick` is
    /// the adapter's monotonic AFC session/retry clock. `monotonic_micros` feeds
    /// bounded probe estimation and is never canonical gameplay state.
    pub fn pump(
        &mut self,
        now: ClientProtocolTime,
    ) -> Result<ClientProtocolPumpReport, ClientProtocolError<W::Error>> {
        self.ensure_active()?;
        let result = self.pump_inner(now);
        self.record_result(result)
    }

    fn pump_inner(
        &mut self,
        now: ClientProtocolTime,
    ) -> Result<ClientProtocolPumpReport, ClientProtocolError<W::Error>> {
        self.observe_clock(now)?;
        let phase_before = self.phase();
        self.service_outbound()?;
        let runtime_report = self.runtime.pump(now.network_tick);
        while let Some(event) = self.runtime.try_next_event() {
            self.handle_event(event, now)?;
        }

        let outcome = self
            .predicted
            .process_pending_authority()
            .map_err(|error| Self::fatal(ClientProtocolFatalError::Prediction(error)))?;
        self.handle_authority_outcome(outcome)?;
        self.expire_resync_if_needed(now.network_tick)?;
        self.try_confirm_result()?;
        self.maybe_schedule_result_repair(now.network_tick)?;
        self.service_outbound()?;

        if self.pending_result.is_some() && !self.result_verified {
            self.metrics.result_wait_pumps = self.metrics.result_wait_pumps.saturating_add(1);
        }
        if runtime_report.abuse == RuntimeAbuseSignal::Disconnect {
            return Err(Self::fatal(
                ClientProtocolFatalError::AuthorityAbuseThreshold,
            ));
        }
        let connection = self.runtime.connection_state();
        if connection != RuntimeConnectionState::Active {
            return Err(Self::fatal(ClientProtocolFatalError::Transport(connection)));
        }

        self.metrics.network_pumps = self.metrics.network_pumps.saturating_add(1);
        let phase_after = self.phase();
        if phase_after != phase_before {
            self.metrics.phase_transitions = self.metrics.phase_transitions.saturating_add(1);
        }
        Ok(ClientProtocolPumpReport {
            runtime: runtime_report,
            phase_before,
            phase_after,
            authority_tick_estimate: self
                .authority_tick_estimate(now.monotonic_micros)
                .map_err(|error| Self::fatal(ClientProtocolFatalError::SessionClock(error)))?,
            result_waiting_for_final_state: self.pending_result.is_some() && !self.result_verified,
            resync_in_flight: self.resync_in_flight,
        })
    }

    /// Predicts one canonical tick and queues one bounded redundant input batch
    /// containing every seat owned by this peer.
    pub fn submit_local_inputs(
        &mut self,
        frames: &[InputFrame],
    ) -> Result<LocalInputSubmitReport, ClientProtocolError<W::Error>> {
        self.ensure_active()?;
        let result = self.submit_local_inputs_inner(frames);
        self.record_result(result)
    }

    fn submit_local_inputs_inner(
        &mut self,
        frames: &[InputFrame],
    ) -> Result<LocalInputSubmitReport, ClientProtocolError<W::Error>> {
        if let Some(reason) = self.resync_in_flight {
            return Err(self.recoverable(ClientProtocolRecoverableError::ResyncInFlight { reason }));
        }
        let phase = self.phase();
        if !matches!(
            phase,
            ConnectionPhase::Countdown | ConnectionPhase::Fighting
        ) {
            return Err(self.recoverable(ClientProtocolRecoverableError::NotFighting { phase }));
        }
        let manifest = self.manifest.ok_or_else(|| {
            Self::fatal(ClientProtocolFatalError::UnexpectedMessage(
                "fighting began without a manifest",
            ))
        })?;
        let expected_tick = self
            .predicted
            .predicted_tick()
            .map(SimTick::next)
            .ok_or_else(|| {
                self.recoverable(ClientProtocolRecoverableError::PredictionNotInitialized)
            })?;
        let scheduled_through = self
            .input_scheduler
            .as_ref()
            .and_then(InputLeadScheduler::last_emitted)
            .unwrap_or(SimTick::ZERO);
        if expected_tick > scheduled_through {
            return Err(
                self.recoverable(ClientProtocolRecoverableError::InputNotDue {
                    next_tick: expected_tick,
                    scheduled_through,
                }),
            );
        }
        let local_seats = manifest
            .ownership
            .as_slice()
            .iter()
            .filter(|assignment| assignment.owner == SeatOwner::Peer(self.peer_id))
            .count();
        if frames.is_empty() || frames.len() != local_seats {
            return Err(Self::fatal(ClientProtocolFatalError::Protocol(
                ProtocolValidationError::InvalidLocalSeatCount,
            )));
        }

        let mut staged = self.input_histories;
        let mut seen_mask = 0_u8;
        let mut prediction_inputs = [None; MAX_SEATS];
        for frame in frames {
            frame
                .validate()
                .map_err(|error| Self::fatal(ClientProtocolFatalError::Protocol(error)))?;
            if frame.tick != expected_tick {
                return Err(Self::fatal(ClientProtocolFatalError::Protocol(
                    ProtocolValidationError::InvalidTickWindow,
                )));
            }
            manifest
                .ownership
                .validate_peer_input(self.peer_id, frame.seat)
                .map_err(|error| Self::fatal(ClientProtocolFatalError::Protocol(error)))?;
            let bit = 1 << frame.seat.get();
            if seen_mask & bit != 0 {
                return Err(Self::fatal(ClientProtocolFatalError::Protocol(
                    ProtocolValidationError::DuplicateInputSeat,
                )));
            }
            seen_mask |= bit;
            if staged[usize::from(frame.seat.get())].push(*frame).is_err() {
                return Err(Self::fatal(ClientProtocolFatalError::Protocol(
                    ProtocolValidationError::NonContiguousInputTicks,
                )));
            }
            prediction_inputs[usize::from(frame.seat.get())] = Some(*frame);
        }
        for assignment in manifest.ownership.as_slice() {
            if assignment.owner == SeatOwner::Peer(self.peer_id)
                && seen_mask & (1 << assignment.seat.get()) == 0
            {
                return Err(Self::fatal(ClientProtocolFatalError::Protocol(
                    ProtocolValidationError::UnownedSeat,
                )));
            }
        }

        let mut windows = [SeatInputWindow::default(); MAX_SEATS];
        let mut window_count = 0_usize;
        let mut redundancy = 0_u8;
        for seat_index in 0..MAX_SEATS {
            if seen_mask & (1 << seat_index) == 0 {
                continue;
            }
            let window = staged[seat_index]
                .window()
                .map_err(|error| Self::fatal(ClientProtocolFatalError::Protocol(error)))?;
            redundancy = redundancy.max(window.len() as u8);
            windows[window_count] = window;
            window_count += 1;
        }
        let mut batch = InputBatch::new(manifest.match_id, self.peer_id, &windows[..window_count])
            .map_err(|error| Self::fatal(ClientProtocolFatalError::Protocol(error)))?;
        if let Some(baseline) = self.predicted.acknowledged_baseline() {
            batch = batch
                .with_state_baseline_ack(baseline.into())
                .map_err(|error| Self::fatal(ClientProtocolFatalError::Protocol(error)))?;
        }
        let queue = match self.runtime.queue_message(WireMessage::InputBatch(batch)) {
            Ok(disposition) => disposition,
            Err(RuntimeQueueError::OutboundQueueFull) => {
                return Err(self.recoverable(ClientProtocolRecoverableError::OutboundBackpressure));
            }
            Err(error) => {
                return Err(Self::fatal(ClientProtocolFatalError::RuntimeQueue(error)));
            }
        };
        let predicted_tick = self
            .predicted
            .predict_next(prediction_inputs)
            .map_err(|error| Self::fatal(ClientProtocolFatalError::Prediction(error)))?;
        debug_assert_eq!(predicted_tick, expected_tick);
        self.input_histories = staged;
        self.metrics.local_input_batches = self.metrics.local_input_batches.saturating_add(1);
        self.metrics.local_input_frames = self
            .metrics
            .local_input_frames
            .saturating_add(frames.len() as u64);
        self.metrics.maximum_local_seats_in_batch = self
            .metrics
            .maximum_local_seats_in_batch
            .max(frames.len() as u8);
        self.metrics.maximum_input_redundancy =
            self.metrics.maximum_input_redundancy.max(redundancy);
        Ok(LocalInputSubmitReport {
            tick: expected_tick,
            seats: frames.len() as u8,
            queue,
        })
    }

    fn handle_event(
        &mut self,
        event: RuntimeEvent,
        now: ClientProtocolTime,
    ) -> Result<(), ClientProtocolError<W::Error>> {
        let RuntimeEvent::Message(message) = event else {
            return match event {
                RuntimeEvent::SessionError(error) => {
                    Err(Self::fatal(ClientProtocolFatalError::Session(error)))
                }
                RuntimeEvent::TransportDisconnected => Err(Self::fatal(
                    ClientProtocolFatalError::Transport(self.runtime.connection_state()),
                )),
                RuntimeEvent::Message(_) => unreachable!(),
            };
        };
        match message {
            WireMessage::Handshake(handshake) => handshake
                .compatibility
                .validate_against(&self.expected_compatibility)
                .map_err(|error| Self::fatal(ClientProtocolFatalError::Protocol(error))),
            WireMessage::Start(StartMessage::Manifest(manifest)) => {
                self.accept_manifest(manifest, now.network_tick)
            }
            WireMessage::Start(StartMessage::Countdown { .. }) => Ok(()),
            WireMessage::ResyncBegin(begin) => self.accept_resync_begin(begin, now.network_tick),
            WireMessage::ResyncChunk(chunk) => self.accept_resync_chunk(chunk, now.network_tick),
            WireMessage::ResyncInputTail(tail) => {
                self.accept_resync_input_tail(tail, now.network_tick)
            }
            WireMessage::ClockReply(reply) => self.accept_clock_reply(reply, now.monotonic_micros),
            WireMessage::CommittedInputRelay(relay) => self.accept_committed_relay(relay),
            WireMessage::StateHashAndAcks(state) => self.accept_state_hash(state),
            WireMessage::StateDeltaAndAcks(delta) => self.accept_state_delta(delta),
            WireMessage::ResultIdentifier(result) => self.accept_result(result, now.network_tick),
            WireMessage::Disconnect(message) => {
                if message.match_id != Some(self.expected_match_id) {
                    return Err(Self::fatal(ClientProtocolFatalError::Protocol(
                        ProtocolValidationError::MatchMismatch,
                    )));
                }
                Err(Self::fatal(ClientProtocolFatalError::AuthorityDisconnect(
                    message,
                )))
            }
            _ => Err(Self::fatal(ClientProtocolFatalError::UnexpectedMessage(
                "message is invalid on the remote client lifecycle",
            ))),
        }
    }

    fn accept_manifest(
        &mut self,
        manifest: MatchManifest,
        now: SimTick,
    ) -> Result<(), ClientProtocolError<W::Error>> {
        if manifest.match_id != self.expected_match_id {
            return Err(Self::fatal(ClientProtocolFatalError::Protocol(
                ProtocolValidationError::MatchMismatch,
            )));
        }
        if self.manifest.is_some() {
            return Err(Self::fatal(ClientProtocolFatalError::UnexpectedMessage(
                "a second manifest reached the client driver",
            )));
        }
        if !manifest.ownership.peer_owns_any_seat(self.peer_id) {
            return Err(Self::fatal(ClientProtocolFatalError::Protocol(
                ProtocolValidationError::UnownedSeat,
            )));
        }
        self.predicted
            .configure_manifest(&manifest)
            .map_err(|error| Self::fatal(ClientProtocolFatalError::Prediction(error)))?;
        self.assembler = Some(
            ClientResyncAssembler::new(
                manifest.match_id,
                self.peer_id,
                self.config.resync_timeout_ticks,
            )
            .map_err(|error| Self::fatal(ClientProtocolFatalError::Resync(error)))?,
        );
        self.manifest = Some(manifest);
        self.last_network_tick = self.last_network_tick.max(now);
        self.metrics.manifests_received = self.metrics.manifests_received.saturating_add(1);
        Ok(())
    }

    fn accept_clock_reply(
        &mut self,
        reply: ClockReply,
        received_micros: u64,
    ) -> Result<(), ClientProtocolError<W::Error>> {
        reply
            .validate()
            .map_err(|error| Self::fatal(ClientProtocolFatalError::Protocol(error)))?;
        if reply.match_id != self.expected_match_id {
            return Err(Self::fatal(ClientProtocolFatalError::Protocol(
                ProtocolValidationError::MatchMismatch,
            )));
        }
        if reply.peer_id != self.peer_id {
            return Err(Self::fatal(ClientProtocolFatalError::Protocol(
                ProtocolValidationError::PeerMismatch,
            )));
        }
        let pending = self.pending_clock_probe.take().ok_or_else(|| {
            Self::fatal(ClientProtocolFatalError::UnexpectedMessage(
                "clock reply arrived without an outstanding probe",
            ))
        })?;
        if pending.probe_id != reply.probe_id {
            return Err(Self::fatal(ClientProtocolFatalError::UnexpectedMessage(
                "clock reply did not match the outstanding probe",
            )));
        }
        let became_synchronized = self
            .authority_clock
            .observe(ClockRoundTripSample {
                probe_id: reply.probe_id.get(),
                sent_micros: pending.sent_micros,
                received_micros,
                authority_tick: reply.authority_tick,
            })
            .map_err(|error| Self::fatal(ClientProtocolFatalError::SessionClock(error)))?;
        self.metrics.clock_replies_accepted = self.metrics.clock_replies_accepted.saturating_add(1);
        if became_synchronized {
            self.metrics.clock_synchronized_transitions = self
                .metrics
                .clock_synchronized_transitions
                .saturating_add(1);
        }
        self.mark_session_clock_synchronized_if_ready()?;
        Ok(())
    }

    fn accept_resync_begin(
        &mut self,
        begin: crate::network_protocol::ResyncBegin,
        now: SimTick,
    ) -> Result<(), ClientProtocolError<W::Error>> {
        if self.resync_in_flight.is_none() {
            return Err(Self::fatal(ClientProtocolFatalError::UnexpectedMessage(
                "authority began an unsolicited resync transfer",
            )));
        }
        let outcome = {
            let assembler = self.assembler.as_mut().ok_or_else(|| {
                Self::fatal(ClientProtocolFatalError::UnexpectedMessage(
                    "resync begin arrived before manifest agreement",
                ))
            })?;
            assembler
                .accept_begin(begin, now)
                .map_err(|error| Self::fatal(ClientProtocolFatalError::Resync(error)))?
        };
        if matches!(outcome, ResyncBeginOutcome::Superseded { .. }) {
            self.metrics.resync_transfer_supersessions =
                self.metrics.resync_transfer_supersessions.saturating_add(1);
        }
        let completed = self
            .assembler
            .as_mut()
            .expect("accepted Begin requires an assembler")
            .apply_staged_chunks(now)
            .map_err(|error| Self::fatal(ClientProtocolFatalError::Resync(error)))?;
        if let Some(completed) = completed {
            self.apply_completed_resync(completed)?;
        }
        Ok(())
    }

    fn accept_resync_chunk(
        &mut self,
        chunk: crate::network_protocol::ResyncChunk,
        now: SimTick,
    ) -> Result<(), ClientProtocolError<W::Error>> {
        if self.resync_in_flight.is_none() {
            return Err(Self::fatal(ClientProtocolFatalError::UnexpectedMessage(
                "authority sent an unsolicited resync chunk",
            )));
        }
        let assembler = self.assembler.as_mut().ok_or_else(|| {
            Self::fatal(ClientProtocolFatalError::UnexpectedMessage(
                "resync chunk arrived before manifest agreement",
            ))
        })?;
        if let ResyncChunkOutcome::Complete(completed) = assembler
            .accept_chunk(chunk, now)
            .map_err(|error| Self::fatal(ClientProtocolFatalError::Resync(error)))?
        {
            self.apply_completed_resync(completed)?;
        }
        Ok(())
    }

    fn accept_resync_input_tail(
        &mut self,
        tail: ResyncInputTail,
        now: SimTick,
    ) -> Result<(), ClientProtocolError<W::Error>> {
        tail.validate()
            .map_err(|error| Self::fatal(ClientProtocolFatalError::Protocol(error)))?;
        if tail.match_id != self.expected_match_id {
            return Err(Self::fatal(ClientProtocolFatalError::Protocol(
                ProtocolValidationError::MatchMismatch,
            )));
        }
        if let Some(previous) = self
            .recent_applied_input_tails
            .iter()
            .flatten()
            .find(|previous| previous.transfer_id == tail.transfer_id)
        {
            if *previous == tail {
                self.metrics.late_duplicate_resync_input_tails = self
                    .metrics
                    .late_duplicate_resync_input_tails
                    .saturating_add(1);
                return Ok(());
            }
            return Err(Self::fatal(ClientProtocolFatalError::Resync(
                ResyncTransferError::ConflictingInputTail {
                    transfer_id: tail.transfer_id,
                },
            )));
        }
        if self.resync_in_flight.is_none() {
            return Err(Self::fatal(ClientProtocolFatalError::UnexpectedMessage(
                "authority sent an unsolicited resync input tail",
            )));
        }
        let assembler = self.assembler.as_mut().ok_or_else(|| {
            Self::fatal(ClientProtocolFatalError::UnexpectedMessage(
                "resync input tail arrived before manifest agreement",
            ))
        })?;
        if let ResyncInputTailOutcome::Complete(completed) = assembler
            .accept_input_tail(tail, now)
            .map_err(|error| Self::fatal(ClientProtocolFatalError::Resync(error)))?
        {
            self.apply_completed_resync(completed)?;
        }
        Ok(())
    }

    fn apply_completed_resync(
        &mut self,
        completed: crate::resync_transfer::CompletedResyncTransfer,
    ) -> Result<(), ClientProtocolError<W::Error>> {
        let reason = self.resync_in_flight.ok_or_else(|| {
            Self::fatal(ClientProtocolFatalError::UnexpectedMessage(
                "a snapshot completed without a tracked resync purpose",
            ))
        })?;
        let manifest = self.manifest.ok_or_else(|| {
            Self::fatal(ClientProtocolFatalError::UnexpectedMessage(
                "a snapshot completed before manifest agreement",
            ))
        })?;
        let contract = snapshot_contract_for_manifest(&manifest);
        self.validate_resync_input_tail(&completed.input_tail, &manifest)?;
        let header = &completed.snapshot.header;
        for (matches, field) in [
            (
                header.protocol_version == contract.protocol_version,
                "protocol version",
            ),
            (
                header.simulation_version == contract.simulation_version,
                "simulation version",
            ),
            (
                header.gameplay_content_hash == contract.gameplay_content_hash,
                "gameplay content hash",
            ),
            (header.match_id == contract.match_id, "match ID"),
            (
                header.master_seed == contract.master_seed,
                "master gameplay seed",
            ),
        ] {
            if !matches {
                return Err(Self::fatal(
                    ClientProtocolFatalError::SnapshotContractMismatch(field),
                ));
            }
        }

        let expected = AppliedCanonicalSnapshot {
            tick: completed.applied.snapshot_tick,
            hash: completed.applied.snapshot_hash,
        };
        let initial = reason == ResyncReason::InitialSync && self.initial_snapshot.is_none();
        let reconnect = reason == ResyncReason::Reconnect;
        let repair = !initial && !reconnect;
        let repair_input_high_water = if repair {
            let mut high_water = None;
            let mut first_owned_seat = true;
            for assignment in manifest.ownership.as_slice() {
                if assignment.owner != SeatOwner::Peer(self.peer_id) {
                    continue;
                }
                let newest = self.input_histories[usize::from(assignment.seat.get())]
                    .newest()
                    .map(|frame| frame.tick);
                if first_owned_seat {
                    high_water = newest;
                    first_owned_seat = false;
                } else if newest != high_water {
                    return Err(Self::fatal(ClientProtocolFatalError::Protocol(
                        ProtocolValidationError::InvalidTickWindow,
                    )));
                }
            }
            high_water
        } else {
            None
        };
        let actual = if initial {
            self.predicted.apply_initial_snapshot(&completed.snapshot)
        } else {
            self.predicted.apply_resync_snapshot(&completed.snapshot)
        }
        .map_err(|error| Self::fatal(ClientProtocolFatalError::Prediction(error)))?;
        if actual != expected {
            return Err(Self::fatal(
                ClientProtocolFatalError::SnapshotApplicationMismatch { expected, actual },
            ));
        }
        self.predicted
            .seed_resync_input_tail(&completed.input_tail)
            .map_err(|error| Self::fatal(ClientProtocolFatalError::Prediction(error)))?;
        if reconnect {
            // A reconnect is an authenticated replacement generation. The
            // authority atomically starts a matching input epoch, so no local
            // redundancy from the detached worker may cross this boundary.
            self.input_histories = [LocalSeatInputHistory::default(); MAX_SEATS];
        }
        let mut scheduler_tick = self.predicted.confirmed_tick().unwrap_or(actual.tick);
        if let Some(high_water) = repair_input_high_water
            && high_water > actual.tick
        {
            let replay_count = high_water.0 - actual.tick.0;
            if replay_count > MAX_INPUT_FRAMES_PER_WINDOW as u64 {
                return Err(Self::fatal(ClientProtocolFatalError::Protocol(
                    ProtocolValidationError::InvalidTickWindow,
                )));
            }
            for value in actual.tick.0 + 1..=high_water.0 {
                let tick = SimTick(value);
                let mut prediction_inputs = [None; MAX_SEATS];
                for assignment in manifest.ownership.as_slice() {
                    if assignment.owner != SeatOwner::Peer(self.peer_id) {
                        continue;
                    }
                    let frame = self.input_histories[usize::from(assignment.seat.get())]
                        .frame_at(tick)
                        .ok_or_else(|| {
                            Self::fatal(ClientProtocolFatalError::Protocol(
                                ProtocolValidationError::InvalidTickWindow,
                            ))
                        })?;
                    prediction_inputs[usize::from(assignment.seat.get())] = Some(frame);
                }
                let replayed = self
                    .predicted
                    .predict_next(prediction_inputs)
                    .map_err(|error| Self::fatal(ClientProtocolFatalError::Prediction(error)))?;
                if replayed != tick {
                    return Err(Self::fatal(ClientProtocolFatalError::Protocol(
                        ProtocolValidationError::InvalidTickWindow,
                    )));
                }
                self.metrics.local_input_ticks_replayed_after_resync = self
                    .metrics
                    .local_input_ticks_replayed_after_resync
                    .saturating_add(1);
            }
            scheduler_tick = high_water;
        }
        self.canonical_snapshot_applied = true;
        self.recent_applied_input_tails[self.recent_applied_input_tail_cursor] =
            Some(completed.input_tail);
        self.recent_applied_input_tail_cursor =
            (self.recent_applied_input_tail_cursor + 1) % MAX_RECENT_APPLIED_RESYNC_TAILS;
        if initial {
            self.initial_snapshot = Some(AppliedInitialSync {
                tick: actual.tick,
                hash: actual.hash,
            });
            self.metrics.initial_snapshots_applied =
                self.metrics.initial_snapshots_applied.saturating_add(1);
        } else {
            self.metrics.hard_resync_snapshots_applied =
                self.metrics.hard_resync_snapshots_applied.saturating_add(1);
        }
        self.input_scheduler = Some(
            InputLeadScheduler::from_confirmed_tick(manifest.input_delay_ticks, scheduler_tick)
                .map_err(|error| Self::fatal(ClientProtocolFatalError::SessionClock(error)))?,
        );
        self.pending_post_sync = Some(PendingClientPostSync {
            applied: completed.applied,
            stage: ClientPostSyncStage::Applied,
            initial,
        });
        if reconnect {
            self.pending_reconnect_resume = Some(AppliedInitialSync {
                tick: actual.tick,
                hash: actual.hash,
            });
        }
        self.try_confirm_result()?;
        Ok(())
    }

    fn accept_committed_relay(
        &mut self,
        relay: CommittedInputRelay,
    ) -> Result<(), ClientProtocolError<W::Error>> {
        self.validate_committed_relay(&relay)?;
        if self.awaiting_reconnect_snapshot() {
            self.metrics.reconnect_replication_deferred = self
                .metrics
                .reconnect_replication_deferred
                .saturating_add(1);
            return Ok(());
        }
        if !self.canonical_snapshot_applied {
            return Err(Self::fatal(ClientProtocolFatalError::UnexpectedMessage(
                "committed input arrived before initial sync",
            )));
        }
        let outcome = self
            .predicted
            .observe_committed_relay(&relay)
            .map_err(|error| Self::fatal(ClientProtocolFatalError::Prediction(error)))?;
        self.metrics.committed_relays = self.metrics.committed_relays.saturating_add(1);
        self.handle_authority_outcome(outcome)?;
        self.try_confirm_result()
    }

    fn accept_state_hash(
        &mut self,
        state: StateHashAndAcks,
    ) -> Result<(), ClientProtocolError<W::Error>> {
        self.validate_state_identity(state.match_id, state.authority_tick, state.state_hash)?;
        self.validate_ack_seats(state.as_slice().iter().map(|ack| ack.seat))?;
        if self.awaiting_reconnect_snapshot() {
            self.metrics.reconnect_replication_deferred = self
                .metrics
                .reconnect_replication_deferred
                .saturating_add(1);
            return Ok(());
        }
        if !self.canonical_snapshot_applied {
            return Err(Self::fatal(ClientProtocolFatalError::UnexpectedMessage(
                "authority state arrived before initial sync",
            )));
        }
        if self.is_stale_state(state.authority_tick) {
            self.metrics.stale_state_messages = self.metrics.stale_state_messages.saturating_add(1);
            return Ok(());
        }
        self.retain_state_identity(state.authority_tick, state.state_hash)?;
        let outcome = self
            .predicted
            .observe_authority_hash(&state)
            .map_err(|error| Self::fatal(ClientProtocolFatalError::Prediction(error)))?;
        self.metrics.state_hash_messages = self.metrics.state_hash_messages.saturating_add(1);
        self.handle_authority_outcome(outcome)?;
        self.try_confirm_result()
    }

    fn accept_state_delta(
        &mut self,
        delta: StateDeltaAndAcks,
    ) -> Result<(), ClientProtocolError<W::Error>> {
        self.validate_state_identity(delta.match_id, delta.authority_tick, delta.state_hash)?;
        self.validate_ack_seats(delta.as_slice().iter().map(|ack| ack.seat))?;
        if self.awaiting_reconnect_snapshot() {
            self.metrics.reconnect_replication_deferred = self
                .metrics
                .reconnect_replication_deferred
                .saturating_add(1);
            return Ok(());
        }
        if !self.canonical_snapshot_applied {
            return Err(Self::fatal(ClientProtocolFatalError::UnexpectedMessage(
                "authority state arrived before initial sync",
            )));
        }
        if self.is_stale_state(delta.authority_tick) {
            self.metrics.stale_state_messages = self.metrics.stale_state_messages.saturating_add(1);
            return Ok(());
        }
        self.retain_state_identity(delta.authority_tick, delta.state_hash)?;
        let outcome = self
            .predicted
            .observe_authority_delta(&delta)
            .map_err(|error| Self::fatal(ClientProtocolFatalError::Prediction(error)))?;
        self.metrics.state_delta_messages = self.metrics.state_delta_messages.saturating_add(1);
        self.handle_authority_outcome(outcome)?;
        self.try_confirm_result()
    }

    fn validate_state_identity(
        &self,
        match_id: MatchId,
        _tick: SimTick,
        _hash: StateHash,
    ) -> Result<(), ClientProtocolError<W::Error>> {
        if match_id != self.expected_match_id {
            return Err(Self::fatal(ClientProtocolFatalError::Protocol(
                ProtocolValidationError::MatchMismatch,
            )));
        }
        Ok(())
    }

    fn validate_ack_seats(
        &self,
        seats: impl Iterator<Item = SeatId>,
    ) -> Result<(), ClientProtocolError<W::Error>> {
        let manifest = self.manifest.expect("initial sync requires a manifest");
        for seat in seats {
            if manifest.ownership.assignment_for_seat(seat).is_none() {
                return Err(Self::fatal(ClientProtocolFatalError::Protocol(
                    ProtocolValidationError::UnownedSeat,
                )));
            }
        }
        Ok(())
    }

    fn is_stale_state(&self, tick: SimTick) -> bool {
        self.latest_state_identity
            .is_some_and(|(latest, _)| tick < latest)
    }

    fn retain_state_identity(
        &mut self,
        tick: SimTick,
        hash: StateHash,
    ) -> Result<(), ClientProtocolError<W::Error>> {
        if let Some((latest_tick, latest_hash)) = self.latest_state_identity
            && latest_tick == tick
            && latest_hash != hash
        {
            return Err(Self::fatal(
                ClientProtocolFatalError::ConflictingStateAtTick {
                    tick,
                    first: latest_hash,
                    second: hash,
                },
            ));
        }
        if self
            .latest_state_identity
            .is_none_or(|(latest, _)| tick >= latest)
        {
            self.latest_state_identity = Some((tick, hash));
        }
        Ok(())
    }

    fn validate_committed_relay(
        &self,
        relay: &CommittedInputRelay,
    ) -> Result<(), ClientProtocolError<W::Error>> {
        relay
            .validate()
            .map_err(|error| Self::fatal(ClientProtocolFatalError::Protocol(error)))?;
        if relay.match_id != self.expected_match_id {
            return Err(Self::fatal(ClientProtocolFatalError::Protocol(
                ProtocolValidationError::MatchMismatch,
            )));
        }
        let manifest = self.manifest.ok_or_else(|| {
            Self::fatal(ClientProtocolFatalError::UnexpectedMessage(
                "committed input arrived before manifest agreement",
            ))
        })?;
        if relay.len() != manifest.ownership.len() {
            return Err(Self::fatal(ClientProtocolFatalError::Protocol(
                ProtocolValidationError::MissingFighterOwner,
            )));
        }
        let mut seen = 0_u8;
        for window in relay.as_slice() {
            let newest = window.newest().ok_or_else(|| {
                Self::fatal(ClientProtocolFatalError::Protocol(
                    ProtocolValidationError::EmptyInputWindow,
                ))
            })?;
            let assignment = manifest
                .ownership
                .assignment_for_seat(newest.frame.seat)
                .ok_or_else(|| {
                    Self::fatal(ClientProtocolFatalError::Protocol(
                        ProtocolValidationError::UnownedSeat,
                    ))
                })?;
            if assignment.fighter != newest.fighter {
                return Err(Self::fatal(ClientProtocolFatalError::Protocol(
                    ProtocolValidationError::MissingFighterOwner,
                )));
            }
            for record in window.as_slice() {
                let source_matches = match (assignment.owner, record.source) {
                    (SeatOwner::Peer(expected), CommittedInputSource::Peer(actual)) => {
                        expected == actual
                    }
                    // A disconnected peer-owned seat may be under canonical
                    // authority-bot substitution without changing the manifest.
                    (_, CommittedInputSource::AuthorityBot) => true,
                    (_, CommittedInputSource::MissingSubstitute) => true,
                    _ => false,
                };
                if !source_matches {
                    return Err(Self::fatal(ClientProtocolFatalError::Protocol(
                        ProtocolValidationError::SeatOwnedByDifferentPeer,
                    )));
                }
            }
            seen |= 1 << newest.frame.seat.get();
        }
        if manifest
            .ownership
            .as_slice()
            .iter()
            .any(|assignment| seen & (1 << assignment.seat.get()) == 0)
        {
            return Err(Self::fatal(ClientProtocolFatalError::Protocol(
                ProtocolValidationError::MissingFighterOwner,
            )));
        }
        Ok(())
    }

    fn validate_resync_input_tail(
        &self,
        tail: &ResyncInputTail,
        manifest: &MatchManifest,
    ) -> Result<(), ClientProtocolError<W::Error>> {
        tail.validate()
            .map_err(|error| Self::fatal(ClientProtocolFatalError::Protocol(error)))?;
        if tail.match_id != self.expected_match_id {
            return Err(Self::fatal(ClientProtocolFatalError::Protocol(
                ProtocolValidationError::MatchMismatch,
            )));
        }
        if tail.len() != manifest.ownership.len() {
            return Err(Self::fatal(ClientProtocolFatalError::Protocol(
                ProtocolValidationError::MissingFighterOwner,
            )));
        }
        let mut seen = 0_u8;
        for window in tail.as_slice() {
            let newest = window.newest().ok_or_else(|| {
                Self::fatal(ClientProtocolFatalError::Protocol(
                    ProtocolValidationError::EmptyInputWindow,
                ))
            })?;
            let assignment = manifest
                .ownership
                .assignment_for_seat(newest.frame.seat)
                .ok_or_else(|| {
                    Self::fatal(ClientProtocolFatalError::Protocol(
                        ProtocolValidationError::UnownedSeat,
                    ))
                })?;
            if assignment.fighter != newest.fighter {
                return Err(Self::fatal(ClientProtocolFatalError::Protocol(
                    ProtocolValidationError::MissingFighterOwner,
                )));
            }
            for record in window.as_slice() {
                let source_matches = match (assignment.owner, record.source) {
                    (SeatOwner::Peer(expected), CommittedInputSource::Peer(actual)) => {
                        expected == actual
                    }
                    // Authority-generated reconnect control is canonical even
                    // while the seat remains peer-owned in the immutable manifest.
                    (_, CommittedInputSource::AuthorityBot) => true,
                    (_, CommittedInputSource::MissingSubstitute) => true,
                    _ => false,
                };
                if !source_matches {
                    return Err(Self::fatal(ClientProtocolFatalError::Protocol(
                        ProtocolValidationError::SeatOwnedByDifferentPeer,
                    )));
                }
            }
            seen |= 1 << newest.frame.seat.get();
        }
        if manifest
            .ownership
            .as_slice()
            .iter()
            .any(|assignment| seen & (1 << assignment.seat.get()) == 0)
        {
            return Err(Self::fatal(ClientProtocolFatalError::Protocol(
                ProtocolValidationError::MissingFighterOwner,
            )));
        }
        Ok(())
    }

    fn accept_result(
        &mut self,
        result: ResultIdentifier,
        now: SimTick,
    ) -> Result<(), ClientProtocolError<W::Error>> {
        result
            .validate()
            .map_err(|error| Self::fatal(ClientProtocolFatalError::Protocol(error)))?;
        if result.match_id != self.expected_match_id {
            return Err(Self::fatal(ClientProtocolFatalError::Protocol(
                ProtocolValidationError::MatchMismatch,
            )));
        }
        if let Some(previous) = self.pending_result {
            if previous != result {
                return Err(Self::fatal(ClientProtocolFatalError::ConflictingResult {
                    first: previous.result_id.get(),
                    second: result.result_id.get(),
                }));
            }
            return Ok(());
        }
        if self.result_verified {
            return Err(Self::fatal(ClientProtocolFatalError::ConflictingResult {
                first: self
                    .runtime
                    .client_session()
                    .and_then(ClientSession::result)
                    .map_or(0, |confirmed| confirmed.result_id),
                second: result.result_id.get(),
            }));
        }
        self.pending_result = Some(result);
        self.pending_result_since = Some(now);
        self.metrics.results_received = self.metrics.results_received.saturating_add(1);
        let before = self.result_verified;
        self.try_confirm_result()?;
        if !before && !self.result_verified {
            self.metrics.results_deferred = self.metrics.results_deferred.saturating_add(1);
        }
        Ok(())
    }

    fn try_confirm_result(&mut self) -> Result<(), ClientProtocolError<W::Error>> {
        if !self.canonical_snapshot_applied {
            return Ok(());
        }
        let Some(result) = self.pending_result else {
            return Ok(());
        };
        let Some(prediction) = self.predicted.prediction() else {
            return Ok(());
        };
        if prediction.confirmed_tick() < result.final_tick {
            return Ok(());
        }
        let Some(hash) = prediction.predicted_hash(result.final_tick).map(StateHash) else {
            return Ok(());
        };
        if hash != result.final_state_hash {
            return Err(Self::fatal(
                ClientProtocolFatalError::FinalStateHashMismatch {
                    tick: result.final_tick,
                    result: result.final_state_hash,
                    confirmed: hash,
                },
            ));
        }
        let confirmed = self
            .runtime
            .client_session()
            .and_then(ClientSession::result)
            .ok_or_else(|| {
                Self::fatal(ClientProtocolFatalError::UnexpectedMessage(
                    "runtime did not bind the accepted result to its client session",
                ))
            })?;
        if confirmed.result_id != result.result_id.get()
            || confirmed.final_tick != result.final_tick
            || confirmed.final_hash != result.final_state_hash
        {
            return Err(Self::fatal(ClientProtocolFatalError::ConflictingResult {
                first: confirmed.result_id,
                second: result.result_id.get(),
            }));
        }
        self.pending_result = None;
        self.pending_result_since = None;
        self.confirmed_result = Some(confirmed);
        self.result_verified = true;
        self.metrics.confirmed_results = self.metrics.confirmed_results.saturating_add(1);
        Ok(())
    }

    fn maybe_schedule_result_repair(
        &mut self,
        now: SimTick,
    ) -> Result<(), ClientProtocolError<W::Error>> {
        let Some(result) = self.pending_result else {
            return Ok(());
        };
        let Some(received_at) = self.pending_result_since else {
            return Ok(());
        };
        if self.result_verified
            || now.get().saturating_sub(received_at.get()) < RESULT_REPAIR_GRACE_TICKS
            || self.resync_in_flight.is_some()
            || self.pending_resync_request.is_some()
            || self.pending_post_sync.is_some()
        {
            return Ok(());
        }
        let Some(prediction) = self.predicted.prediction() else {
            return Ok(());
        };
        let last_confirmed_tick = prediction.confirmed_tick();
        let last_confirmed_hash = StateHash(
            prediction
                .predicted_hash(last_confirmed_tick)
                .unwrap_or_default(),
        );
        let reason = match prediction.predicted_hash(result.final_tick) {
            Some(hash) if hash != result.final_state_hash.0 => ResyncReason::HashMismatch,
            _ => ResyncReason::HistoryExpired,
        };
        self.schedule_resync(reason, last_confirmed_tick, last_confirmed_hash)?;
        self.metrics.hard_resync_requests = self.metrics.hard_resync_requests.saturating_add(1);
        self.metrics.result_repair_requests = self.metrics.result_repair_requests.saturating_add(1);
        Ok(())
    }

    fn awaiting_reconnect_snapshot(&self) -> bool {
        self.resync_in_flight == Some(ResyncReason::Reconnect) && !self.canonical_snapshot_applied
    }

    fn handle_authority_outcome(
        &mut self,
        outcome: ClientAuthorityOutcome,
    ) -> Result<(), ClientProtocolError<W::Error>> {
        match outcome {
            ClientAuthorityOutcome::Matched { .. } => {
                self.metrics.matched_authority_states =
                    self.metrics.matched_authority_states.saturating_add(1);
            }
            ClientAuthorityOutcome::Corrected { .. } => {
                self.metrics.rollback_corrections =
                    self.metrics.rollback_corrections.saturating_add(1);
            }
            ClientAuthorityOutcome::HardResyncRequired {
                reason,
                last_confirmed_tick,
                last_confirmed_hash,
            } if self.resync_in_flight.is_none()
                && self.pending_post_sync.is_none()
                && self.assembler.as_ref().is_none_or(|assembler| {
                    assembler.staged_pre_begin_chunks() == 0
                        && assembler.staged_pre_begin_input_tails() == 0
                }) =>
            {
                self.schedule_resync(reason, last_confirmed_tick, last_confirmed_hash)?;
                self.metrics.hard_resync_requests =
                    self.metrics.hard_resync_requests.saturating_add(1);
            }
            _ => {}
        }
        Ok(())
    }

    fn schedule_resync(
        &mut self,
        reason: ResyncReason,
        last_confirmed_tick: SimTick,
        last_confirmed_hash: StateHash,
    ) -> Result<(), ClientProtocolError<W::Error>> {
        let assembler = self.assembler.as_ref().ok_or_else(|| {
            Self::fatal(ClientProtocolFatalError::UnexpectedMessage(
                "resync was requested before manifest agreement",
            ))
        })?;
        if self.resync_in_flight.is_some() {
            return Ok(());
        }
        self.pending_resync_request =
            Some(assembler.make_request(reason, last_confirmed_tick, last_confirmed_hash));
        self.resync_in_flight = Some(reason);
        self.metrics.resync_requests = self.metrics.resync_requests.saturating_add(1);
        Ok(())
    }

    fn expire_resync_if_needed(
        &mut self,
        now: SimTick,
    ) -> Result<(), ClientProtocolError<W::Error>> {
        let Some(assembler) = self.assembler.as_mut() else {
            return Ok(());
        };
        let expired = assembler
            .expire_if_timed_out(now)
            .map_err(|error| Self::fatal(ClientProtocolFatalError::Resync(error)))?;
        if expired.is_none() {
            return Ok(());
        }
        self.metrics.resync_transfer_timeouts =
            self.metrics.resync_transfer_timeouts.saturating_add(1);
        let reason = self.resync_in_flight.ok_or_else(|| {
            Self::fatal(ClientProtocolFatalError::UnexpectedMessage(
                "an untracked resync transfer timed out",
            ))
        })?;
        let (tick, hash) = if reason == ResyncReason::InitialSync {
            (SimTick::ZERO, StateHash(0))
        } else {
            let prediction = self.predicted.prediction().ok_or_else(|| {
                Self::fatal(ClientProtocolFatalError::UnexpectedMessage(
                    "hard resync timed out before prediction initialization",
                ))
            })?;
            let tick = prediction.confirmed_tick();
            let hash = StateHash(prediction.predicted_hash(tick).unwrap_or_default());
            (tick, hash)
        };
        self.pending_resync_request = Some(assembler.make_request(reason, tick, hash));
        Ok(())
    }

    fn service_outbound(&mut self) -> Result<(), ClientProtocolError<W::Error>> {
        if let Some(request) = self.pending_resync_request.take() {
            match self
                .runtime
                .queue_message(WireMessage::ResyncRequest(request))
            {
                Ok(_) => return Ok(()),
                Err(RuntimeQueueError::OutboundQueueFull) => {
                    self.pending_resync_request = Some(request);
                    self.metrics.outbound_backpressure =
                        self.metrics.outbound_backpressure.saturating_add(1);
                    return Ok(());
                }
                Err(error) => {
                    return Err(Self::fatal(ClientProtocolFatalError::RuntimeQueue(error)));
                }
            }
        }

        if self.pending_post_sync.is_none()
            && let Some(sync) = self.pending_reconnect_resume
        {
            self.mark_session_clock_synchronized_if_ready()?;
            let synchronized = self
                .runtime
                .client_session()
                .expect("remote client runtime always owns a client session")
                .is_clock_synchronized();
            if !synchronized {
                self.try_queue_clock_probe()?;
                return Ok(());
            }
            self.runtime
                .client_session_mut()
                .expect("remote client runtime always owns a client session")
                .complete_reconnect(sync, self.last_network_tick)
                .map_err(|error| Self::fatal(ClientProtocolFatalError::Session(error)))?;
            self.pending_reconnect_resume = None;
            self.resync_in_flight = None;
        }

        let Some(mut pending) = self.pending_post_sync.take() else {
            self.try_queue_clock_probe()?;
            return Ok(());
        };

        if pending.stage == ClientPostSyncStage::Ready {
            self.mark_session_clock_synchronized_if_ready()?;
            let synchronized = self
                .runtime
                .client_session()
                .expect("remote client runtime always owns a client session")
                .is_clock_synchronized();
            if !synchronized {
                self.pending_post_sync = Some(pending);
                self.try_queue_clock_probe()?;
                return Ok(());
            }
        }
        let queue_result = match pending.stage {
            ClientPostSyncStage::Applied => self
                .runtime
                .queue_message(WireMessage::ResyncApplied(pending.applied)),
            ClientPostSyncStage::InitialSync => {
                let session = *self
                    .runtime
                    .client_session()
                    .expect("remote client runtime always owns a client session");
                let mut preview = session;
                let message = preview
                    .apply_initial_sync(
                        self.expected_match_id,
                        pending.applied.snapshot_tick,
                        pending.applied.snapshot_hash,
                        self.last_network_tick,
                    )
                    .map_err(|error| Self::fatal(ClientProtocolFatalError::Session(error)))?;
                match self.runtime.queue_start_message(message) {
                    Ok(disposition) => {
                        *self
                            .runtime
                            .client_session_mut()
                            .expect("remote client runtime always owns a client session") = preview;
                        Ok(disposition)
                    }
                    Err(error) => Err(error),
                }
            }
            ClientPostSyncStage::Ready => {
                let ready = self
                    .runtime
                    .client_session()
                    .expect("remote client runtime always owns a client session")
                    .ready_message()
                    .map_err(|error| Self::fatal(ClientProtocolFatalError::Session(error)))?;
                self.runtime.queue_start_message(ready)
            }
        };

        match queue_result {
            Ok(_) => match pending.stage {
                ClientPostSyncStage::Applied if pending.initial => {
                    pending.stage = ClientPostSyncStage::InitialSync;
                    self.pending_post_sync = Some(pending);
                }
                ClientPostSyncStage::Applied => {
                    if self.pending_reconnect_resume.is_none() {
                        self.resync_in_flight = None;
                    }
                }
                ClientPostSyncStage::InitialSync => {
                    pending.stage = ClientPostSyncStage::Ready;
                    self.pending_post_sync = Some(pending);
                }
                ClientPostSyncStage::Ready => {
                    self.resync_in_flight = None;
                }
            },
            Err(RuntimeQueueError::OutboundQueueFull) => {
                self.pending_post_sync = Some(pending);
                self.metrics.outbound_backpressure =
                    self.metrics.outbound_backpressure.saturating_add(1);
            }
            Err(error) => {
                return Err(Self::fatal(ClientProtocolFatalError::RuntimeQueue(error)));
            }
        }
        Ok(())
    }

    fn try_queue_clock_probe(&mut self) -> Result<bool, ClientProtocolError<W::Error>> {
        if self.manifest.is_none() || self.pending_clock_probe.is_some() {
            return Ok(false);
        }
        let periodic = self.authority_clock.is_synchronized();
        if periodic
            && self.last_clock_probe_sent_micros.is_some_and(|last| {
                self.last_monotonic_micros.saturating_sub(last)
                    < self.config.clock_probe_interval_micros
            })
        {
            return Ok(false);
        }
        let probe_id = ClockProbeId::new(self.next_clock_probe_id)
            .map_err(|error| Self::fatal(ClientProtocolFatalError::Protocol(error)))?;
        let probe = ClockProbe {
            match_id: self.expected_match_id,
            peer_id: self.peer_id,
            probe_id,
        };
        match self.runtime.queue_message(WireMessage::ClockProbe(probe)) {
            Ok(_) => {
                self.pending_clock_probe = Some(PendingClockProbe {
                    probe_id,
                    sent_micros: self.last_monotonic_micros,
                });
                self.last_clock_probe_sent_micros = Some(self.last_monotonic_micros);
                self.next_clock_probe_id = self
                    .next_clock_probe_id
                    .checked_add(1)
                    .ok_or_else(|| Self::fatal(ClientProtocolFatalError::TimelineExhausted))?;
                self.metrics.clock_probes_queued =
                    self.metrics.clock_probes_queued.saturating_add(1);
                if periodic {
                    self.metrics.periodic_clock_refreshes =
                        self.metrics.periodic_clock_refreshes.saturating_add(1);
                }
                Ok(true)
            }
            Err(RuntimeQueueError::OutboundQueueFull) => {
                self.metrics.outbound_backpressure =
                    self.metrics.outbound_backpressure.saturating_add(1);
                Ok(false)
            }
            Err(error) => Err(Self::fatal(ClientProtocolFatalError::RuntimeQueue(error))),
        }
    }

    fn mark_session_clock_synchronized_if_ready(
        &mut self,
    ) -> Result<(), ClientProtocolError<W::Error>> {
        if !self.authority_clock.is_synchronized() {
            return Ok(());
        }
        let session = self
            .runtime
            .client_session_mut()
            .expect("remote client runtime always owns a client session");
        let synchronizable = session.phase() == ConnectionPhase::Ready
            || (session.phase() == ConnectionPhase::InitialSync
                && session.countdown_start_tick().is_some());
        if synchronizable && !session.is_clock_synchronized() {
            session
                .mark_clock_synchronized()
                .map_err(|error| Self::fatal(ClientProtocolFatalError::Session(error)))?;
        }
        Ok(())
    }

    fn observe_clock(
        &mut self,
        now: ClientProtocolTime,
    ) -> Result<(), ClientProtocolError<W::Error>> {
        if now.network_tick < self.last_network_tick {
            return Err(Self::fatal(ClientProtocolFatalError::ClockRegressed {
                previous: self.last_network_tick,
                received: now.network_tick,
            }));
        }
        if now.monotonic_micros < self.last_monotonic_micros {
            return Err(Self::fatal(
                ClientProtocolFatalError::MonotonicClockRegressed {
                    previous_micros: self.last_monotonic_micros,
                    received_micros: now.monotonic_micros,
                },
            ));
        }
        self.last_network_tick = now.network_tick;
        self.last_monotonic_micros = now.monotonic_micros;
        Ok(())
    }

    fn ensure_active(&self) -> Result<(), ClientProtocolError<W::Error>> {
        match self.failure {
            Some(fault) => Err(Self::fatal(ClientProtocolFatalError::AlreadyFailed(fault))),
            None => Ok(()),
        }
    }

    fn recoverable(
        &mut self,
        error: ClientProtocolRecoverableError,
    ) -> ClientProtocolError<W::Error> {
        self.metrics.recoverable_errors = self.metrics.recoverable_errors.saturating_add(1);
        if error == ClientProtocolRecoverableError::OutboundBackpressure {
            self.metrics.outbound_backpressure =
                self.metrics.outbound_backpressure.saturating_add(1);
        }
        ClientProtocolError::Recoverable(error)
    }

    const fn fatal(error: ClientProtocolFatalError<W::Error>) -> ClientProtocolError<W::Error> {
        ClientProtocolError::Fatal(error)
    }

    fn record_result<R>(
        &mut self,
        result: Result<R, ClientProtocolError<W::Error>>,
    ) -> Result<R, ClientProtocolError<W::Error>> {
        if let Err(ClientProtocolError::Fatal(error)) = &result {
            if !matches!(error, ClientProtocolFatalError::AlreadyFailed(_)) {
                self.failure.get_or_insert(error.fault());
                self.metrics.fatal_errors = self.metrics.fatal_errors.saturating_add(1);
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority_input::{AbuseSignal, AuthorityInputCollector, AuthorityInputConfig};
    use crate::determinism::{FighterId, SimEntityKind};
    use crate::network_codec::{ResultId, StateHashAndAcks};
    use crate::network_io::InProcessEndpoint;
    use crate::network_protocol::{
        AuthorityKind, BuildId, CommittedInputRecord, CommittedSeatInputWindow, DefinitionId,
        FighterSlotConfig, GameplayContentHash, InputButtons, InputSequence, ManifestHash,
        ProtocolVersion, QuantizedAxis, ReplayFormatVersion, SIMULATION_HZ, SeatAssignment,
        SeatOwnership, SimulationVersion, TeamId, TransferId,
    };
    use crate::network_runtime::NetworkRuntime;
    use crate::resync_transfer::AuthorityResyncTransfer;
    use crate::rollback::RollbackWorld;
    use crate::session::AuthoritySessionGate;
    use crate::snapshot::{
        ArenaRuntimeSnapshot, FighterSnapshot, MatchPhaseSnapshot, MatchStateSnapshot,
        MatchStatsSnapshot, PoolAllocatorSnapshot, SnapshotError, SnapshotHeader,
    };
    use crate::state_sync::{AuthorityDeltaOutcome, AuthoritySnapshotHistory, StateBaseline};

    const MATCH_BYTES: [u8; 16] = *b"remote-client-v1";
    const START_TICK: SimTick = SimTick(20);

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum TestWorldError {
        TickGap,
        WrongSeat,
        Snapshot(SnapshotError),
    }

    #[derive(Clone)]
    struct TestWorld {
        snapshot: CanonicalSnapshot,
    }

    impl TestWorld {
        fn initial(manifest: &MatchManifest) -> Self {
            let allocators = SimEntityKind::ALL
                .into_iter()
                .map(|kind| PoolAllocatorSnapshot::empty(kind, 1).unwrap())
                .collect();
            let fighters = FighterId::ALL.map(|fighter| FighterSnapshot {
                occupied: fighter.index() < 2,
                active: fighter.index() < 2,
                ..FighterSnapshot::empty(fighter)
            });
            Self {
                snapshot: CanonicalSnapshot {
                    header: SnapshotHeader::new(
                        u32::from(manifest.compatibility.simulation.get()),
                        u32::from(manifest.compatibility.protocol.get()),
                        crate::headless::snapshot_gameplay_content_hash(
                            manifest.compatibility.gameplay_content,
                        ),
                        *manifest.match_id.as_bytes(),
                        SimTick::ZERO,
                        manifest.master_gameplay_seed,
                    ),
                    match_state: MatchStateSnapshot {
                        phase: MatchPhaseSnapshot::Fight,
                        active_slots_mask: 0b0011,
                        stocks: [3, 3, 0, 0],
                        ..MatchStateSnapshot::default()
                    },
                    fighters,
                    arena: ArenaRuntimeSnapshot::default(),
                    allocators,
                    dynamic_objects: Vec::new(),
                    rng_streams: Vec::new(),
                    stats: MatchStatsSnapshot::default(),
                },
            }
        }
    }

    impl RollbackWorld for TestWorld {
        type Snapshot = CanonicalSnapshot;
        type Error = TestWorldError;

        fn current_tick(&self) -> SimTick {
            self.snapshot.header.tick
        }

        fn capture_snapshot(&self) -> Result<Self::Snapshot, Self::Error> {
            Ok(self.snapshot.clone())
        }

        fn restore_snapshot(&mut self, snapshot: &Self::Snapshot) -> Result<(), Self::Error> {
            self.snapshot = snapshot.clone();
            Ok(())
        }

        fn step(
            &mut self,
            tick: SimTick,
            inputs: &[InputFrame; MAX_SEATS],
        ) -> Result<(), Self::Error> {
            if tick != self.snapshot.header.tick.next() {
                return Err(TestWorldError::TickGap);
            }
            for (seat, frame) in inputs.iter().enumerate() {
                if frame.tick != tick || usize::from(frame.seat.get()) != seat {
                    return Err(TestWorldError::WrongSeat);
                }
                self.snapshot.stats.damage_by_fighter[seat] = self.snapshot.stats.damage_by_fighter
                    [seat]
                    .wrapping_add(i32::from(frame.movement_x.get()) + 128);
            }
            self.snapshot.header.tick = tick;
            self.snapshot.stats.gameplay_ticks = tick.get();
            Ok(())
        }

        fn state_hash(&self) -> Result<u64, Self::Error> {
            self.snapshot
                .canonical_hash()
                .map_err(TestWorldError::Snapshot)
        }
    }

    fn compatibility() -> crate::network_protocol::CompatibilityId {
        crate::network_protocol::CompatibilityId {
            protocol: ProtocolVersion::new(1).unwrap(),
            simulation: SimulationVersion::new(1).unwrap(),
            replay: ReplayFormatVersion::new(1).unwrap(),
            build: BuildId::new([1; 16]).unwrap(),
            gameplay_content: GameplayContentHash::new([2; 32]).unwrap(),
        }
    }

    fn match_id() -> MatchId {
        MatchId::new(MATCH_BYTES).unwrap()
    }

    fn peer_id() -> PeerId {
        PeerId::new(41).unwrap()
    }

    fn client_time(tick: u64) -> ClientProtocolTime {
        ClientProtocolTime {
            network_tick: SimTick(tick),
            monotonic_micros: u128::from(tick)
                .saturating_mul(1_000_000)
                .div_ceil(u128::from(SIMULATION_HZ)) as u64,
        }
    }

    fn manifest() -> MatchManifest {
        let ownership = SeatOwnership::from_assignments(&[
            SeatAssignment {
                seat: SeatId::new(0).unwrap(),
                fighter: FighterId::new(0).unwrap(),
                owner: SeatOwner::Peer(peer_id()),
            },
            SeatAssignment {
                seat: SeatId::new(1).unwrap(),
                fighter: FighterId::new(1).unwrap(),
                owner: SeatOwner::Peer(peer_id()),
            },
        ])
        .unwrap();
        let mut slots = [FighterSlotConfig::default(); MAX_SEATS];
        for (index, slot) in slots.iter_mut().take(2).enumerate() {
            *slot = FighterSlotConfig {
                occupied: true,
                fighter: FighterId::new(index as u8).unwrap(),
                team: TeamId::new(index as u8).unwrap(),
                character: DefinitionId::new(index as u16 + 1).unwrap(),
                style: DefinitionId::new(1).unwrap(),
                equipment: DefinitionId::new(0).unwrap(),
            };
        }
        MatchManifest {
            compatibility: compatibility(),
            manifest_hash: ManifestHash(0xC11E_1701),
            match_id: match_id(),
            authority: AuthorityKind::Listen,
            trusted_results: false,
            arena: DefinitionId::new(1).unwrap(),
            rules: DefinitionId::new(1).unwrap(),
            slots,
            ownership,
            master_gameplay_seed: 0xAFC0_4411,
            rng_scheme_version: 1,
            tick_rate_hz: SIMULATION_HZ,
            input_delay_ticks: 2,
            rollback_limit_ticks: 12,
            snapshot_history_ticks: 32,
            agreed_start_tick: START_TICK,
        }
    }

    fn frame(tick: u64, seat: usize, movement: i8) -> InputFrame {
        InputFrame {
            tick: SimTick(tick),
            seat: SeatId::new(seat as u8).unwrap(),
            movement_x: QuantizedAxis::new(movement).unwrap(),
            held_buttons: InputButtons::new(InputButtons::LIGHT).unwrap(),
            pressed_buttons: InputButtons::new(InputButtons::LIGHT).unwrap(),
            sequence: InputSequence(tick as u16),
            ..InputFrame::default()
        }
    }

    fn local_frames(tick: u64) -> [InputFrame; 2] {
        [frame(tick, 0, 17), frame(tick, 1, -11)]
    }

    fn all_prediction_frames(local: [InputFrame; 2]) -> [InputFrame; MAX_SEATS] {
        std::array::from_fn(|seat| {
            if seat < 2 {
                local[seat]
            } else {
                frame(local[0].tick.get(), seat, 0)
            }
        })
    }

    fn committed_relay(local: [InputFrame; 2]) -> CommittedInputRelay {
        let windows = std::array::from_fn::<_, 2, _>(|seat| {
            CommittedSeatInputWindow::from_newest_first(&[CommittedInputRecord {
                frame: local[seat],
                fighter: FighterId::new(seat as u8).unwrap(),
                source: CommittedInputSource::Peer(peer_id()),
            }])
            .unwrap()
        });
        CommittedInputRelay::new(match_id(), local[0].tick, &windows).unwrap()
    }

    struct TestTransfer {
        transfer: AuthorityResyncTransfer,
        begin_sent: bool,
        input_tail_sent: bool,
        next_chunk: u16,
    }

    /// Test-only authority protocol peer. It deliberately owns only its
    /// authority runtime and transfer state; no LocalLoopbackMatch or lab
    /// coordinator participates in these tests.
    struct TestAuthority {
        runtime: NetworkRuntime<InProcessEndpoint>,
        manifest: MatchManifest,
        snapshot: CanonicalSnapshot,
        transfer: Option<TestTransfer>,
        received_inputs: Vec<InputBatch>,
        resync_applied: u64,
        startup_messages: u64,
        next_transfer: u32,
    }

    impl TestAuthority {
        fn new(
            endpoint: InProcessEndpoint,
            manifest: MatchManifest,
            snapshot: CanonicalSnapshot,
        ) -> Self {
            let gate = AuthoritySessionGate::new(manifest).unwrap();
            let mut runtime = NetworkRuntime::new_authority(
                endpoint,
                manifest.compatibility,
                gate,
                peer_id(),
                RuntimeConfig::default(),
            )
            .unwrap();
            runtime
                .queue_start_message(StartMessage::Manifest(manifest))
                .unwrap();
            Self {
                runtime,
                manifest,
                snapshot,
                transfer: None,
                received_inputs: Vec::new(),
                resync_applied: 0,
                startup_messages: 0,
                next_transfer: 1,
            }
        }

        fn new_reconnect(
            endpoint: InProcessEndpoint,
            manifest: MatchManifest,
            snapshot: CanonicalSnapshot,
        ) -> Self {
            let gate = AuthoritySessionGate::new(manifest).unwrap();
            let runtime = NetworkRuntime::new_authority(
                endpoint,
                manifest.compatibility,
                gate,
                peer_id(),
                RuntimeConfig::default(),
            )
            .unwrap();
            let request = crate::network_protocol::ResyncRequest {
                match_id: manifest.match_id,
                peer_id: peer_id(),
                reason: ResyncReason::Reconnect,
                last_confirmed_tick: SimTick::ZERO,
                last_confirmed_hash: StateHash(0),
            };
            let mut windows = [CommittedSeatInputWindow::default(); MAX_SEATS];
            for (index, assignment) in manifest.ownership.as_slice().iter().enumerate() {
                windows[index] =
                    CommittedSeatInputWindow::from_newest_first(&[CommittedInputRecord {
                        frame: InputFrame {
                            tick: snapshot.header.tick,
                            seat: assignment.seat,
                            held_buttons: InputButtons::new(InputButtons::GUARD).unwrap(),
                            ..InputFrame::default()
                        },
                        fighter: assignment.fighter,
                        source: CommittedInputSource::Peer(peer_id()),
                    }])
                    .unwrap();
            }
            let transfer = AuthorityResyncTransfer::from_snapshot(
                request,
                TransferId::new(1).unwrap(),
                &snapshot,
                &windows[..manifest.ownership.len()],
            )
            .unwrap();
            Self {
                runtime,
                manifest,
                snapshot,
                transfer: Some(TestTransfer {
                    transfer,
                    begin_sent: false,
                    input_tail_sent: false,
                    next_chunk: 0,
                }),
                received_inputs: Vec::new(),
                resync_applied: 0,
                startup_messages: 0,
                next_transfer: 2,
            }
        }

        fn pump(&mut self, now: SimTick) {
            self.service_transfer();
            self.runtime.pump(now);
            while let Some(event) = self.runtime.try_next_event() {
                let RuntimeEvent::Message(message) = event else {
                    panic!("test authority runtime failed: {event:?}");
                };
                match message {
                    WireMessage::Handshake(_) => {}
                    WireMessage::Start(
                        StartMessage::ManifestAccepted { .. }
                        | StartMessage::InitialSyncApplied { .. }
                        | StartMessage::Ready { .. },
                    ) => {
                        self.startup_messages = self.startup_messages.saturating_add(1);
                    }
                    WireMessage::ResyncRequest(request) => {
                        assert!(self.transfer.is_none());
                        let transfer_id = TransferId::new(self.next_transfer).unwrap();
                        self.next_transfer += 1;
                        let mut windows = [CommittedSeatInputWindow::default(); MAX_SEATS];
                        for (index, assignment) in
                            self.manifest.ownership.as_slice().iter().enumerate()
                        {
                            windows[index] = CommittedSeatInputWindow::from_newest_first(&[
                                CommittedInputRecord {
                                    frame: InputFrame {
                                        tick: self.snapshot.header.tick,
                                        seat: assignment.seat,
                                        ..InputFrame::default()
                                    },
                                    fighter: assignment.fighter,
                                    source: CommittedInputSource::MissingSubstitute,
                                },
                            ])
                            .unwrap();
                        }
                        let transfer = AuthorityResyncTransfer::from_snapshot(
                            request,
                            transfer_id,
                            &self.snapshot,
                            &windows[..self.manifest.ownership.len()],
                        )
                        .unwrap();
                        self.transfer = Some(TestTransfer {
                            transfer,
                            begin_sent: false,
                            input_tail_sent: false,
                            next_chunk: 0,
                        });
                    }
                    WireMessage::ResyncApplied(applied) => {
                        let mut transfer = self.transfer.take().unwrap();
                        transfer.transfer.validate_applied(&applied).unwrap();
                        self.resync_applied += 1;
                    }
                    // NetworkRuntime has already validated this identity and
                    // reserved the matching reply at the current authority tick.
                    WireMessage::ClockProbe(_) => {}
                    WireMessage::InputBatch(batch) => self.received_inputs.push(batch),
                    WireMessage::Disconnect(disconnect) => {
                        panic!("test client disconnected: {disconnect:?}")
                    }
                    other => panic!("unexpected test authority message: {other:?}"),
                }
            }
            self.service_transfer();
        }

        fn service_transfer(&mut self) {
            let Some(pending) = self.transfer.as_mut() else {
                return;
            };
            if !pending.begin_sent {
                if self
                    .runtime
                    .queue_message(WireMessage::ResyncBegin(pending.transfer.begin()))
                    .is_ok()
                {
                    pending.begin_sent = true;
                }
                return;
            }
            if !pending.input_tail_sent {
                if self
                    .runtime
                    .queue_message(WireMessage::ResyncInputTail(pending.transfer.input_tail()))
                    .is_ok()
                {
                    pending.input_tail_sent = true;
                }
                return;
            }
            if pending.next_chunk >= pending.transfer.begin().chunk_count {
                return;
            }
            let chunk = pending
                .transfer
                .chunks_from(pending.next_chunk)
                .unwrap()
                .next()
                .unwrap();
            if self
                .runtime
                .queue_message(WireMessage::ResyncChunk(chunk))
                .is_ok()
            {
                pending.next_chunk += 1;
            }
        }

        fn queue(&mut self, message: WireMessage) {
            self.runtime.queue_message(message).unwrap();
        }

        fn take_inputs(&mut self) -> Vec<InputBatch> {
            core::mem::take(&mut self.received_inputs)
        }
    }

    type TestClient = RemotePredictedClientProtocol<
        InProcessEndpoint,
        TestWorld,
        NoopEventDiscard,
        NoopRollbackTiming,
    >;

    fn connected_pair() -> (TestClient, TestAuthority, CanonicalSnapshot) {
        let manifest = manifest();
        let initial = TestWorld::initial(&manifest).snapshot;
        let predicted =
            PredictedClient::new(TestWorld::initial(&manifest), match_id(), 32).unwrap();
        let (client_endpoint, authority_endpoint) = InProcessEndpoint::pair(128).unwrap();
        let mut client = RemotePredictedClientProtocol::new(
            client_endpoint,
            match_id(),
            peer_id(),
            compatibility(),
            predicted,
            ClientProtocolConfig::default(),
            client_time(0),
        )
        .unwrap();
        let mut authority = TestAuthority::new(authority_endpoint, manifest, initial.clone());
        let mut marked_loaded = false;

        for _ in 0..512 {
            client.pump(client_time(0)).unwrap();
            authority.pump(SimTick::ZERO);
            client.pump(client_time(0)).unwrap();
            if client.phase() == ConnectionPhase::Loading && !marked_loaded {
                client.mark_content_loaded(client_time(0)).unwrap();
                marked_loaded = true;
            }
            authority.pump(SimTick::ZERO);
            if client.phase() == ConnectionPhase::Countdown {
                break;
            }
        }
        assert!(marked_loaded);
        assert_eq!(client.phase(), ConnectionPhase::Countdown);
        assert_eq!(client.initial_snapshot().unwrap().tick, SimTick::ZERO);
        assert_eq!(authority.resync_applied, 1);
        assert!(client.is_clock_synchronized());
        assert!(client.metrics().clock_probes_queued >= 3);
        assert!(
            authority
                .runtime
                .authority_gate()
                .unwrap()
                .peer(peer_id())
                .unwrap()
                .clock_probe_count
                >= 3
        );

        let countdown_start_tick = client
            .runtime
            .client_session()
            .and_then(ClientSession::countdown_start_tick)
            .unwrap();
        assert!(countdown_start_tick >= START_TICK);
        let first_fighting_network_tick = countdown_start_tick.next();
        client
            .pump(client_time(first_fighting_network_tick.get()))
            .unwrap();
        authority.pump(first_fighting_network_tick);
        assert_eq!(client.phase(), ConnectionPhase::Fighting);
        (client, authority, initial)
    }

    #[test]
    fn authority_disconnect_preserves_exact_fields_only_for_the_expected_match() {
        use crate::network_protocol::{DisconnectCode, RetryDisposition};

        let exact = DisconnectMessage {
            match_id: Some(match_id()),
            code: DisconnectCode::ServerShutdown,
            retry: RetryDisposition::MatchEndedNoContest,
            detail_code: 0x41FC,
            last_confirmed_tick: Some(SimTick(19)),
        };
        let (mut client, mut authority, _) = connected_pair();
        let now = client.last_network_tick.next();
        authority.queue(WireMessage::Disconnect(exact));
        authority.pump(now);
        let outcome = client.pump(client_time(now.get()));
        assert!(
            matches!(
                &outcome,
                Err(ClientProtocolError::Fatal(
                    ClientProtocolFatalError::AuthorityDisconnect(received)
                )) if *received == exact
            ),
            "unexpected disconnect outcome: {outcome:?}"
        );

        for rejected_match in [None, Some(MatchId::new(*b"other-match-id!!").unwrap())] {
            let (mut client, mut authority, _) = connected_pair();
            let now = client.last_network_tick.next();
            authority.queue(WireMessage::Disconnect(DisconnectMessage {
                match_id: rejected_match,
                ..exact
            }));
            authority.pump(now);
            assert!(matches!(
                client.pump(client_time(now.get())),
                Err(ClientProtocolError::Fatal(
                    ClientProtocolFatalError::Protocol(ProtocolValidationError::MatchMismatch)
                ))
            ));
        }
    }

    fn actual_network_tick(client: &TestClient, proposed_timeline_tick: SimTick) -> SimTick {
        let actual_start = client
            .runtime
            .client_session()
            .and_then(ClientSession::countdown_start_tick)
            .unwrap();
        SimTick(actual_start.get() + proposed_timeline_tick.get() - START_TICK.get())
    }

    fn deliver_client_outbound(
        client: &mut TestClient,
        authority: &mut TestAuthority,
        now: SimTick,
    ) {
        let now = actual_network_tick(client, now);
        client.pump(client_time(now.get())).unwrap();
        authority.pump(now);
    }

    fn deliver_authority_outbound(
        client: &mut TestClient,
        authority: &mut TestAuthority,
        now: SimTick,
    ) {
        let now = actual_network_tick(client, now);
        authority.pump(now);
        client.pump(client_time(now.get())).unwrap();
        authority.pump(now);
    }

    fn authorize_initial_inputs(client: &mut TestClient) -> DueInputTicks {
        client.take_due_input_ticks().unwrap().unwrap()
    }

    #[test]
    fn independent_authority_completes_startup_and_couch_input_redundancy() {
        let (mut client, mut authority, initial) = connected_pair();
        assert_eq!(client.predicted_client().world().snapshot, initial);
        assert!(matches!(
            client.submit_local_inputs(&local_frames(1)),
            Err(ClientProtocolError::Recoverable(
                ClientProtocolRecoverableError::InputNotDue {
                    next_tick: SimTick(1),
                    scheduled_through: SimTick::ZERO,
                }
            ))
        ));
        assert_eq!(
            authorize_initial_inputs(&mut client),
            DueInputTicks {
                first: SimTick(1),
                last: SimTick(4),
            }
        );

        let tick_one = local_frames(1);
        let submitted = client.submit_local_inputs(&tick_one).unwrap();
        assert_eq!(submitted.tick, SimTick(1));
        assert_eq!(submitted.seats, 2);
        deliver_client_outbound(&mut client, &mut authority, SimTick(21));
        let first = authority.take_inputs();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].len(), 2);
        assert!(first[0].state_baseline_ack().is_some());
        assert!(first[0].as_slice().iter().all(|window| window.len() == 1));

        let mut canonical = TestWorld { snapshot: initial };
        canonical
            .step(SimTick(1), &all_prediction_frames(tick_one))
            .unwrap();
        authority.queue(WireMessage::CommittedInputRelay(committed_relay(tick_one)));
        authority.queue(WireMessage::StateHashAndAcks(
            StateHashAndAcks::new(
                match_id(),
                SimTick(1),
                StateHash(canonical.state_hash().unwrap()),
                &[],
            )
            .unwrap(),
        ));
        deliver_authority_outbound(&mut client, &mut authority, SimTick(21));
        assert_eq!(client.predicted_client().confirmed_tick(), Some(SimTick(1)));

        let tick_two = local_frames(2);
        client.submit_local_inputs(&tick_two).unwrap();
        deliver_client_outbound(&mut client, &mut authority, SimTick(22));
        let second = authority.take_inputs();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].len(), 2);
        assert!(second[0].as_slice().iter().all(|window| {
            window.len() == 2
                && window.newest().unwrap().tick == SimTick(2)
                && window.as_slice()[1].tick == SimTick(1)
        }));
        assert_eq!(client.metrics().maximum_local_seats_in_batch, 2);
        assert_eq!(client.metrics().maximum_input_redundancy, 2);
    }

    #[test]
    fn reconnect_bootstrap_accepts_unsolicited_transfer_without_startup_replay() {
        let manifest = manifest();
        let initial = TestWorld::initial(&manifest).snapshot;
        let mut predicted =
            PredictedClient::new(TestWorld::initial(&manifest), match_id(), 32).unwrap();
        predicted.apply_initial_snapshot(&initial).unwrap();

        let mut authority_world = TestWorld { snapshot: initial };
        for tick in 1..=5 {
            authority_world
                .step(SimTick(tick), &all_prediction_frames(local_frames(tick)))
                .unwrap();
        }
        let (client_endpoint, authority_endpoint) = InProcessEndpoint::pair(128).unwrap();
        let reconnect_at = client_time(125);
        let mut client = RemotePredictedClientProtocol::new_reconnect(
            client_endpoint,
            manifest,
            peer_id(),
            SimTick(120),
            predicted,
            ClientProtocolConfig::default(),
            reconnect_at,
        )
        .unwrap();
        let mut authority =
            TestAuthority::new_reconnect(authority_endpoint, manifest, authority_world.snapshot);
        authority.queue(WireMessage::CommittedInputRelay(committed_relay(
            local_frames(5),
        )));
        authority.queue(WireMessage::StateHashAndAcks(
            StateHashAndAcks::new(
                match_id(),
                SimTick(5),
                StateHash(authority.snapshot.canonical_hash().unwrap()),
                &[],
            )
            .unwrap(),
        ));

        assert_eq!(client.phase(), ConnectionPhase::InitialSync);
        assert_eq!(client.take_due_input_ticks().unwrap(), None);
        assert!(matches!(
            client.submit_local_inputs(&local_frames(1)),
            Err(ClientProtocolError::Recoverable(
                ClientProtocolRecoverableError::ResyncInFlight {
                    reason: ResyncReason::Reconnect
                }
            ))
        ));

        for offset in 0..512 {
            let now = client_time(125 + offset);
            client.pump(now).unwrap();
            authority.pump(now.network_tick);
            client.pump(now).unwrap();
            authority.pump(now.network_tick);
            if client.phase() == ConnectionPhase::Fighting && authority.resync_applied == 1 {
                break;
            }
        }

        assert_eq!(client.phase(), ConnectionPhase::Fighting);
        assert!(client.is_clock_synchronized());
        assert_eq!(client.countdown_start_tick(), Some(SimTick(120)));
        assert_eq!(client.resync_in_flight, None);
        assert_eq!(client.predicted_client().confirmed_tick(), Some(SimTick(5)));
        assert_eq!(
            client
                .predicted_client()
                .snapshot_at(SimTick(5))
                .unwrap()
                .header
                .tick,
            SimTick(5)
        );
        assert!(client.metrics().reconnect_replication_deferred >= 2);
        assert_ne!(
            client
                .predicted_client()
                .prediction()
                .unwrap()
                .input_boundary()[0]
                .held_buttons
                .bits()
                & InputButtons::GUARD,
            0
        );
        assert_eq!(authority.resync_applied, 1);
        assert_eq!(authority.startup_messages, 0);
    }

    #[test]
    fn client_requested_repair_replays_sent_future_input_without_reauthoring_its_generation() {
        let (mut client, mut authority, snapshot) = connected_pair();
        let resync_requests_before = client.metrics().resync_requests;
        authorize_initial_inputs(&mut client);
        let tick_one = local_frames(1);
        client.submit_local_inputs(&tick_one).unwrap();
        deliver_client_outbound(&mut client, &mut authority, SimTick(21));
        let manifest = manifest();
        let initial_batches = authority.take_inputs();
        assert_eq!(initial_batches.len(), 1);
        let mut authority_inputs = AuthorityInputCollector::new(
            manifest.match_id,
            manifest.ownership,
            SimTick(1),
            AuthorityInputConfig::default(),
        )
        .unwrap();
        let initial_report = authority_inputs
            .ingest_peer_batch(peer_id(), &initial_batches[0])
            .unwrap();
        assert_eq!(initial_report.accepted, 2);
        assert_eq!(initial_report.rejected, 0);

        client
            .schedule_resync(
                ResyncReason::HistoryExpired,
                SimTick::ZERO,
                StateHash(snapshot.canonical_hash().unwrap()),
            )
            .unwrap();
        deliver_client_outbound(&mut client, &mut authority, SimTick(21));
        assert!(authority.transfer.is_some());
        for round in 0..64 {
            deliver_authority_outbound(&mut client, &mut authority, SimTick(22 + round));
            if client.resync_in_flight.is_none() {
                break;
            }
        }

        assert_eq!(client.failure(), None);
        assert_eq!(client.resync_in_flight, None);
        assert_eq!(client.metrics().resync_requests, resync_requests_before + 1);
        assert_eq!(client.metrics().hard_resync_snapshots_applied, 1);
        assert_eq!(client.metrics().local_input_ticks_replayed_after_resync, 1);
        assert_eq!(client.predicted_client().predicted_tick(), Some(SimTick(1)));

        let due = client.take_due_input_ticks().unwrap().unwrap();
        assert_eq!(due.first, SimTick(2));
        let mut changed_tick = local_frames(2);
        for frame in &mut changed_tick {
            frame.held_buttons = InputButtons::new(InputButtons::HEAVY).unwrap();
            frame.pressed_buttons = InputButtons::new(InputButtons::HEAVY).unwrap();
        }
        client.submit_local_inputs(&changed_tick).unwrap();
        deliver_client_outbound(&mut client, &mut authority, SimTick(100));
        let post_repair = authority.take_inputs();
        assert_eq!(post_repair.len(), 1);
        for window in post_repair[0].as_slice() {
            let newest = window.newest().unwrap();
            let seat = usize::from(newest.seat.get());
            assert_eq!(window.len(), 2);
            assert_eq!(*newest, changed_tick[seat]);
            assert_eq!(window.as_slice()[1], tick_one[seat]);
        }

        let repaired_report = authority_inputs
            .ingest_peer_batch(peer_id(), &post_repair[0])
            .unwrap();
        assert_eq!(repaired_report.accepted, 2);
        assert_eq!(repaired_report.rejections.duplicate, 2);
        assert_eq!(repaired_report.rejections.invalid, 0);
        assert_eq!(repaired_report.rejections.future, 0);
        assert_eq!(repaired_report.rejections.sequence, 0);
        assert_eq!(repaired_report.rejections.conflicting, 0);
        assert_eq!(
            authority_inputs.take_abuse_signal(peer_id()),
            AbuseSignal::None
        );
        for seat in 0..2 {
            let cursor = authority_inputs
                .high_water(SeatId::new(seat).unwrap())
                .accepted
                .unwrap();
            assert_eq!(cursor.tick, SimTick(2));
            assert_eq!(cursor.sequence, InputSequence(2));
        }
    }

    #[test]
    fn unsolicited_repair_begin_fails_closed_while_fighting() {
        let (mut client, _authority, snapshot) = connected_pair();
        let manifest = manifest();
        let request = crate::network_protocol::ResyncRequest {
            match_id: manifest.match_id,
            peer_id: peer_id(),
            reason: ResyncReason::HistoryExpired,
            last_confirmed_tick: SimTick::ZERO,
            last_confirmed_hash: StateHash(0),
        };
        let mut windows = [CommittedSeatInputWindow::default(); MAX_SEATS];
        for (index, assignment) in manifest.ownership.as_slice().iter().enumerate() {
            windows[index] = CommittedSeatInputWindow::from_newest_first(&[CommittedInputRecord {
                frame: InputFrame {
                    tick: snapshot.header.tick,
                    seat: assignment.seat,
                    ..InputFrame::default()
                },
                fighter: assignment.fighter,
                source: CommittedInputSource::Peer(peer_id()),
            }])
            .unwrap();
        }
        let transfer = AuthorityResyncTransfer::from_snapshot(
            request,
            TransferId::new(99).unwrap(),
            &snapshot,
            &windows[..manifest.ownership.len()],
        )
        .unwrap();

        assert!(matches!(
            client.accept_resync_begin(transfer.begin(), SimTick(21)),
            Err(ClientProtocolError::Fatal(
                ClientProtocolFatalError::UnexpectedMessage(
                    "authority began an unsolicited resync transfer"
                )
            ))
        ));
    }

    #[test]
    fn late_applied_tail_duplicate_is_idempotent_but_conflict_fails_closed() {
        let (mut client, mut authority, _) = connected_pair();
        let tail = client
            .recent_applied_input_tails
            .iter()
            .flatten()
            .copied()
            .next()
            .unwrap();

        // Model a delayed reliable retry that crosses after ResyncApplied and
        // the Countdown -> Fighting transition.
        authority.queue(WireMessage::ResyncInputTail(tail));
        deliver_authority_outbound(&mut client, &mut authority, SimTick(21));
        assert_eq!(client.phase(), ConnectionPhase::Fighting);
        assert_eq!(client.metrics().late_duplicate_resync_input_tails, 1);

        let conflicting = ResyncInputTail::from_parts(
            tail.match_id,
            tail.transfer_id,
            tail.snapshot_tick,
            StateHash(tail.snapshot_hash.0 ^ 1),
            tail.recent_input_start,
            tail.recent_input_end,
            tail.as_slice(),
        )
        .unwrap();
        let now = actual_network_tick(&client, SimTick(21));
        assert!(matches!(
            client.accept_resync_input_tail(conflicting, now),
            Err(ClientProtocolError::Fatal(
                ClientProtocolFatalError::Resync(ResyncTransferError::ConflictingInputTail { .. })
            ))
        ));
    }

    #[test]
    fn final_relay_before_final_state_waits_for_exact_delta_and_exposes_result_once() {
        let (mut client, mut authority, initial) = connected_pair();
        authorize_initial_inputs(&mut client);
        let final_inputs = local_frames(1);
        client.submit_local_inputs(&final_inputs).unwrap();
        deliver_client_outbound(&mut client, &mut authority, SimTick(21));
        assert_eq!(authority.take_inputs().len(), 1);

        let mut canonical = TestWorld {
            snapshot: initial.clone(),
        };
        canonical
            .step(SimTick(1), &all_prediction_frames(final_inputs))
            .unwrap();
        let final_snapshot = canonical.snapshot.clone();
        let final_hash = StateHash(final_snapshot.canonical_hash().unwrap());

        let mut state_history = AuthoritySnapshotHistory::new(match_id(), 2).unwrap();
        let initial_baseline: StateBaseline = state_history.record_snapshot(&initial).unwrap();
        state_history.record_snapshot(&final_snapshot).unwrap();
        let delta = match state_history
            .build_latest_delta(initial_baseline, &[])
            .unwrap()
        {
            AuthorityDeltaOutcome::Delta(delta) => delta,
            AuthorityDeltaOutcome::FullResyncRequired(reason) => {
                panic!("sparse test delta unexpectedly required resync: {reason:?}")
            }
        };

        authority.queue(WireMessage::CommittedInputRelay(committed_relay(
            final_inputs,
        )));
        authority.queue(WireMessage::ResultIdentifier(ResultIdentifier {
            match_id: match_id(),
            result_id: ResultId::new(0xF1A1).unwrap(),
            final_tick: SimTick(1),
            final_state_hash: final_hash,
        }));
        deliver_authority_outbound(&mut client, &mut authority, SimTick(21));

        assert_eq!(client.phase(), ConnectionPhase::ConfirmingResult);
        assert_eq!(
            client.predicted_client().confirmed_tick(),
            Some(SimTick::ZERO)
        );
        assert_eq!(client.take_confirmed_result(), None);
        assert_eq!(client.metrics().results_deferred, 1);

        authority.queue(WireMessage::StateDeltaAndAcks(delta));
        deliver_authority_outbound(&mut client, &mut authority, SimTick(22));

        assert_eq!(client.phase(), ConnectionPhase::Results);
        let confirmed = client.take_confirmed_result().unwrap();
        assert_eq!(confirmed.result_id, 0xF1A1);
        assert_eq!(confirmed.final_tick, SimTick(1));
        assert_eq!(confirmed.final_hash, final_hash);
        assert_eq!(client.take_confirmed_result(), None);
        assert_eq!(client.metrics().confirmed_results, 1);
        assert_eq!(client.metrics().state_delta_messages, 1);
    }

    #[test]
    fn final_state_before_delayed_final_relay_waits_then_confirms_deterministically() {
        let (mut client, mut authority, initial) = connected_pair();
        authorize_initial_inputs(&mut client);
        let final_inputs = local_frames(1);
        client.submit_local_inputs(&final_inputs).unwrap();
        deliver_client_outbound(&mut client, &mut authority, SimTick(21));
        assert_eq!(authority.take_inputs().len(), 1);

        let mut canonical = TestWorld {
            snapshot: initial.clone(),
        };
        canonical
            .step(SimTick(1), &all_prediction_frames(final_inputs))
            .unwrap();
        let final_snapshot = canonical.snapshot.clone();
        let final_hash = StateHash(final_snapshot.canonical_hash().unwrap());
        let mut state_history = AuthoritySnapshotHistory::new(match_id(), 2).unwrap();
        let initial_baseline: StateBaseline = state_history.record_snapshot(&initial).unwrap();
        state_history.record_snapshot(&final_snapshot).unwrap();
        let delta = match state_history
            .build_latest_delta(initial_baseline, &[])
            .unwrap()
        {
            AuthorityDeltaOutcome::Delta(delta) => delta,
            AuthorityDeltaOutcome::FullResyncRequired(reason) => {
                panic!("sparse test delta unexpectedly required resync: {reason:?}")
            }
        };
        let result = ResultIdentifier {
            match_id: match_id(),
            result_id: ResultId::new(0xF1A2).unwrap(),
            final_tick: SimTick(1),
            final_state_hash: final_hash,
        };

        authority.queue(WireMessage::StateDeltaAndAcks(delta));
        deliver_authority_outbound(&mut client, &mut authority, SimTick(21));
        assert_eq!(
            client.predicted_client().confirmed_tick(),
            Some(SimTick::ZERO)
        );
        assert_eq!(client.take_confirmed_result(), None);

        authority.queue(WireMessage::ResultIdentifier(result));
        deliver_authority_outbound(&mut client, &mut authority, SimTick(22));
        assert_eq!(client.phase(), ConnectionPhase::ConfirmingResult);
        assert_eq!(client.take_confirmed_result(), None);

        // Model the final unreliable relay being dropped or delayed across the
        // independently ordered State and Control channels.
        authority.queue(WireMessage::CommittedInputRelay(committed_relay(
            final_inputs,
        )));
        deliver_authority_outbound(&mut client, &mut authority, SimTick(23));

        assert_eq!(client.phase(), ConnectionPhase::Results);
        assert_eq!(
            client.take_confirmed_result(),
            Some(ConfirmedSessionResult {
                result_id: result.result_id.get(),
                final_tick: result.final_tick,
                final_hash: result.final_state_hash,
            })
        );
        assert_eq!(client.take_confirmed_result(), None);
        assert_eq!(client.metrics().confirmed_results, 1);
    }

    #[test]
    fn routes_normal_rollback_then_applies_a_bounded_hard_resync() {
        let (mut client, mut authority, initial) = connected_pair();
        authorize_initial_inputs(&mut client);
        let predicted_inputs = local_frames(1);
        client.submit_local_inputs(&predicted_inputs).unwrap();
        deliver_client_outbound(&mut client, &mut authority, SimTick(21));
        authority.take_inputs();

        let corrected_inputs = [predicted_inputs[0], frame(1, 1, 39)];
        let mut corrected_world = TestWorld {
            snapshot: initial.clone(),
        };
        corrected_world
            .step(SimTick(1), &all_prediction_frames(corrected_inputs))
            .unwrap();
        authority.queue(WireMessage::CommittedInputRelay(committed_relay(
            corrected_inputs,
        )));
        authority.queue(WireMessage::StateHashAndAcks(
            StateHashAndAcks::new(
                match_id(),
                SimTick(1),
                StateHash(corrected_world.state_hash().unwrap()),
                &[],
            )
            .unwrap(),
        ));
        deliver_authority_outbound(&mut client, &mut authority, SimTick(21));
        assert_eq!(client.metrics().rollback_corrections, 1);
        assert_eq!(client.predicted_client().confirmed_tick(), Some(SimTick(1)));
        assert_eq!(
            client.predicted_client().world().state_hash().unwrap(),
            corrected_world.state_hash().unwrap()
        );

        let tick_two = local_frames(2);
        client.submit_local_inputs(&tick_two).unwrap();
        deliver_client_outbound(&mut client, &mut authority, SimTick(22));
        authority.take_inputs();

        let mut hard_resync_world = TestWorld { snapshot: initial };
        for tick in 1..=20 {
            hard_resync_world
                .step(SimTick(tick), &all_prediction_frames(local_frames(tick)))
                .unwrap();
        }
        authority.snapshot = hard_resync_world.snapshot.clone();
        authority.queue(WireMessage::CommittedInputRelay(committed_relay(
            local_frames(20),
        )));
        deliver_authority_outbound(&mut client, &mut authority, SimTick(22));
        assert_eq!(client.metrics().hard_resync_requests, 1);

        for _ in 0..512 {
            let now = actual_network_tick(&client, SimTick(22));
            client.pump(client_time(now.get())).unwrap();
            authority.pump(now);
            if client.metrics().hard_resync_snapshots_applied == 1 && authority.resync_applied == 2
            {
                break;
            }
        }
        assert_eq!(client.metrics().hard_resync_snapshots_applied, 1);
        assert_eq!(authority.resync_applied, 2);
        assert_eq!(
            client.predicted_client().predicted_tick(),
            Some(SimTick(20))
        );
        assert_eq!(
            client.predicted_client().world().state_hash().unwrap(),
            hard_resync_world.state_hash().unwrap()
        );
        assert_eq!(client.phase(), ConnectionPhase::Fighting);
    }
}
