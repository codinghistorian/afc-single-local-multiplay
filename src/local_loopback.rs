//! Bounded local-match orchestration over AFC's real packet/session boundary.
//!
//! Offline and same-machine multiplayer must not call the simulation directly
//! from a client. This runner owns a client runtime and an authority runtime joined
//! by [`InProcessEndpoint`], so handshake, manifest agreement, initial full sync,
//! redundant inputs, state acknowledgements, and results all traverse the same
//! codec and channel implementation used by socket transports.

use std::fmt;

use crate::authority::{
    AuthorityMatch, AuthorityMatchError, AuthoritySimulation, AuthoritySnapshot,
    AuthorityTickReport,
};
use crate::authority_input::{AuthorityInputConfig, AuthorityInputOrigin, InputIngestReport};
use crate::network_codec::{
    Handshake, ProcessedInputAck, ResultId, ResultIdentifier, StateDeltaAndAcks, StateHashAndAcks,
    WireMessage,
};
use crate::network_io::{
    DEFAULT_IN_PROCESS_QUEUE_PACKETS, InProcessConfigError, InProcessEndpoint,
    MAX_IN_PROCESS_QUEUE_PACKETS,
};
use crate::network_protocol::{
    ClockProbeId, CommittedInputRecord, CommittedInputRelay, CommittedInputSource,
    CommittedSeatInputWindow, ConnectionPhase, InputBatch, InputFrame, MAX_INPUT_FRAMES_PER_WINDOW,
    MAX_RESYNC_INPUT_TAIL_TICKS, MAX_SEATS, MatchManifest, PeerId, ProtocolValidationError,
    ResyncInputTail, ResyncReason, SeatId, SeatInputWindow, SeatOwner, SimTick, StartMessage,
    StateBaselineAck, StateHash, TransferId,
};
use crate::network_runtime::{
    NetworkRuntime, QueueDisposition, RuntimeConfig, RuntimeConfigError, RuntimeConnectionState,
    RuntimeEvent, RuntimeQueueError,
};
use crate::replay::{AuthorityReplayRecorder, Replay, ReplayError};
use crate::resync_transfer::{
    AuthorityResyncTransfer, ClientResyncAssembler, ResyncBeginOutcome, ResyncChunkOutcome,
    ResyncInputTailOutcome, ResyncTransferError,
};
use crate::rollback::RollbackWorld;
use crate::session::{
    AppliedInitialSync, AuthoritySessionGate, ClientSession, ConfirmedSessionResult, SessionError,
    SessionTimeouts,
};
use crate::session_clock::MIN_CLOCK_SYNC_SAMPLES;
use crate::snapshot::{CanonicalSnapshot, SnapshotError};
use crate::state_sync::{
    AuthoritySnapshotHistory as AuthorityStateHistory, AuthorityStateSyncCoordinator,
    DEFAULT_STATE_SYNC_HISTORY_ENTRIES, PeerStateSyncError, PeerStateUpdateOutcome, StateSyncError,
};

pub const LOCAL_STATE_HASH_HISTORY_TICKS: usize = 128;
pub const DEFAULT_STARTUP_NETWORK_ROUND_LIMIT: u16 = 1_024;
pub const MAX_STARTUP_NETWORK_ROUND_LIMIT: u16 = 8_192;
pub const LOCAL_REPLAY_HASH_INTERVAL_TICKS: u64 = 60;
pub const LOCAL_REPLAY_KEYFRAME_INTERVAL_TICKS: u64 = 600;
pub const DEFAULT_STARTUP_COUNTDOWN_TICK_LIMIT: u32 = 600;
pub const MAX_STARTUP_COUNTDOWN_TICK_LIMIT: u32 = 36_000;
pub const DEFAULT_STATE_DELTA_INTERVAL_TICKS: u8 = 3;
pub const MAX_STATE_DELTA_INTERVAL_TICKS: u8 = 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalLoopbackConfig {
    pub endpoint_capacity_packets: usize,
    pub runtime: RuntimeConfig,
    /// Work bound for handshake plus a maximum-size initial snapshot transfer.
    /// Network rounds do not advance the canonical session clock.
    pub startup_network_round_limit: u16,
    /// Upper bound for the convenience [`LocalLoopbackMatch::start`] countdown
    /// loop. Long-running launchers may instead pump the countdown themselves.
    pub startup_countdown_tick_limit: u32,
    /// Authoritative snapshot-delta cadence. Hashes and committed inputs remain
    /// 60 Hz; the default 3-tick interval emits deltas at 20 Hz.
    pub state_delta_interval_ticks: u8,
}

impl Default for LocalLoopbackConfig {
    fn default() -> Self {
        Self {
            endpoint_capacity_packets: DEFAULT_IN_PROCESS_QUEUE_PACKETS,
            runtime: RuntimeConfig::default(),
            startup_network_round_limit: DEFAULT_STARTUP_NETWORK_ROUND_LIMIT,
            startup_countdown_tick_limit: DEFAULT_STARTUP_COUNTDOWN_TICK_LIMIT,
            state_delta_interval_ticks: DEFAULT_STATE_DELTA_INTERVAL_TICKS,
        }
    }
}

impl LocalLoopbackConfig {
    pub fn validate(self) -> Result<(), LocalLoopbackConfigError> {
        self.runtime
            .validate()
            .map_err(LocalLoopbackConfigError::Runtime)?;
        if self.endpoint_capacity_packets == 0
            || self.endpoint_capacity_packets > MAX_IN_PROCESS_QUEUE_PACKETS
        {
            return Err(LocalLoopbackConfigError::EndpointCapacity);
        }
        if self.startup_network_round_limit == 0
            || self.startup_network_round_limit > MAX_STARTUP_NETWORK_ROUND_LIMIT
        {
            return Err(LocalLoopbackConfigError::StartupRoundLimit);
        }
        if self.startup_countdown_tick_limit == 0
            || self.startup_countdown_tick_limit > MAX_STARTUP_COUNTDOWN_TICK_LIMIT
        {
            return Err(LocalLoopbackConfigError::CountdownTickLimit);
        }
        if self.state_delta_interval_ticks == 0
            || self.state_delta_interval_ticks > MAX_STATE_DELTA_INTERVAL_TICKS
        {
            return Err(LocalLoopbackConfigError::StateDeltaInterval);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalLoopbackConfigError {
    Runtime(RuntimeConfigError),
    EndpointCapacity,
    StartupRoundLimit,
    CountdownTickLimit,
    StateDeltaInterval,
}

impl fmt::Display for LocalLoopbackConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid local loopback configuration: {self:?}")
    }
}

impl std::error::Error for LocalLoopbackConfigError {}

/// Verified outcome returned by a client world after installing the decoded
/// canonical snapshot. The runner compares this with transfer metadata before it
/// acknowledges readiness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppliedCanonicalSnapshot {
    pub tick: SimTick,
    pub hash: StateHash,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientAuthorityOutcome {
    Passive,
    Matched {
        tick: SimTick,
    },
    Corrected {
        authoritative_tick: SimTick,
        resimulated_through: SimTick,
    },
    Ignored,
    AwaitingAuthoritativeSnapshot {
        tick: SimTick,
    },
    AwaitingCommittedInputs {
        tick: SimTick,
    },
    HardResyncRequired {
        reason: ResyncReason,
        last_confirmed_tick: SimTick,
        last_confirmed_hash: StateHash,
    },
}

/// Client-world seam used by the loopback runner. A production predicted world
/// can implement this directly; every [`RollbackWorld`] using canonical snapshots
/// receives the implementation automatically.
pub trait InitialSnapshotTarget {
    type Error;

    fn apply_initial_snapshot(
        &mut self,
        snapshot: &CanonicalSnapshot,
    ) -> Result<AppliedCanonicalSnapshot, Self::Error>;

    fn configure_match(&mut self, _manifest: &MatchManifest) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Exact canonical byte baseline that this client can reconstruct deltas
    /// from. Merely receiving a hash is not enough to acknowledge a baseline.
    fn state_baseline_ack(&self) -> Option<StateBaselineAck> {
        None
    }

    fn observe_authority_hash(
        &mut self,
        _message: &StateHashAndAcks,
    ) -> Result<ClientAuthorityOutcome, Self::Error> {
        Ok(ClientAuthorityOutcome::Passive)
    }

    fn observe_authority_delta(
        &mut self,
        _message: &StateDeltaAndAcks,
    ) -> Result<ClientAuthorityOutcome, Self::Error> {
        Ok(ClientAuthorityOutcome::Passive)
    }

    fn observe_committed_inputs(
        &mut self,
        _message: &CommittedInputRelay,
    ) -> Result<ClientAuthorityOutcome, Self::Error> {
        Ok(ClientAuthorityOutcome::Passive)
    }

    fn seed_resync_input_tail(&mut self, _tail: &ResyncInputTail) -> Result<(), Self::Error> {
        Ok(())
    }

    fn poll_authority(&mut self) -> Result<ClientAuthorityOutcome, Self::Error> {
        Ok(ClientAuthorityOutcome::Passive)
    }

    /// Applies a reliable correction after startup. Prediction targets override
    /// this to reset rollback and delta histories atomically.
    fn apply_resync_snapshot(
        &mut self,
        snapshot: &CanonicalSnapshot,
    ) -> Result<AppliedCanonicalSnapshot, Self::Error> {
        self.apply_initial_snapshot(snapshot)
    }
}

impl<T> InitialSnapshotTarget for T
where
    T: RollbackWorld<Snapshot = CanonicalSnapshot>,
{
    type Error = T::Error;

    fn apply_initial_snapshot(
        &mut self,
        snapshot: &CanonicalSnapshot,
    ) -> Result<AppliedCanonicalSnapshot, Self::Error> {
        self.restore_snapshot(snapshot)?;
        Ok(AppliedCanonicalSnapshot {
            tick: self.current_tick(),
            hash: StateHash(self.state_hash()?),
        })
    }
}

/// Minimal useful client target for offline/headless operation and protocol tests.
#[derive(Clone, Debug, Default)]
pub struct CanonicalSnapshotMirror {
    snapshot: Option<CanonicalSnapshot>,
    baseline: Option<StateBaselineAck>,
}

impl CanonicalSnapshotMirror {
    pub const fn snapshot(&self) -> Option<&CanonicalSnapshot> {
        self.snapshot.as_ref()
    }
}

impl InitialSnapshotTarget for CanonicalSnapshotMirror {
    type Error = SnapshotError;

    fn apply_initial_snapshot(
        &mut self,
        snapshot: &CanonicalSnapshot,
    ) -> Result<AppliedCanonicalSnapshot, Self::Error> {
        let applied = AppliedCanonicalSnapshot {
            tick: snapshot.header.tick,
            hash: StateHash(snapshot.canonical_hash()?),
        };
        self.snapshot = Some(snapshot.clone());
        self.baseline = Some(StateBaselineAck {
            tick: applied.tick,
            hash: applied.hash,
        });
        Ok(applied)
    }

    fn state_baseline_ack(&self) -> Option<StateBaselineAck> {
        self.baseline
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalLoopbackFault {
    Protocol,
    Session,
    Runtime,
    Resync,
    Simulation,
    ClientSnapshot,
    SnapshotMismatch,
    UnexpectedMessage,
    Capacity,
}

#[derive(Debug)]
pub enum LocalLoopbackError<SimulationError, ClientError> {
    Config(LocalLoopbackConfigError),
    Endpoint(InProcessConfigError),
    RuntimeConfig(RuntimeConfigError),
    Protocol(ProtocolValidationError),
    Session(SessionError),
    RuntimeQueue(RuntimeQueueError),
    Resync(ResyncTransferError),
    StateSync(StateSyncError),
    PeerStateSync(PeerStateSyncError),
    Snapshot(SnapshotError),
    Replay(ReplayError),
    Authority(AuthorityMatchError<SimulationError>),
    ClientSnapshot(ClientError),
    UnsupportedOwnership,
    InitialSnapshotAfterStart {
        snapshot_tick: SimTick,
        start_tick: SimTick,
    },
    InitialSnapshotMismatch {
        expected: AppliedCanonicalSnapshot,
        actual: AppliedCanonicalSnapshot,
    },
    InitialSnapshotContractMismatch(&'static str),
    StateHashMismatch {
        tick: SimTick,
        authority: StateHash,
        received: StateHash,
    },
    ConflictingStateHash {
        tick: SimTick,
        first: StateHash,
        second: StateHash,
    },
    RegressedStateTick {
        latest: SimTick,
        received: SimTick,
    },
    ConflictingResult {
        first: u64,
        second: u64,
    },
    MatchAlreadyFinished,
    UnexpectedMessage(&'static str),
    UnexpectedPhase {
        expected: ConnectionPhase,
        actual: ConnectionPhase,
    },
    InvalidInputSet,
    NonContiguousLocalInput {
        seat: SeatId,
        previous_tick: SimTick,
        received_tick: SimTick,
    },
    RejectedInput(InputIngestReport),
    StartupBudgetExceeded,
    TimelineExhausted,
    TransportDisconnected,
    Failed(LocalLoopbackFault),
}

impl<SE, CE> LocalLoopbackError<SE, CE> {
    fn fault(&self) -> LocalLoopbackFault {
        match self {
            Self::Protocol(_) | Self::InvalidInputSet | Self::NonContiguousLocalInput { .. } => {
                LocalLoopbackFault::Protocol
            }
            Self::Session(_) | Self::UnexpectedPhase { .. } | Self::MatchAlreadyFinished => {
                LocalLoopbackFault::Session
            }
            Self::RuntimeQueue(_) | Self::TransportDisconnected => LocalLoopbackFault::Runtime,
            Self::Resync(_) | Self::StateSync(_) | Self::PeerStateSync(_) => {
                LocalLoopbackFault::Resync
            }
            Self::Authority(_) | Self::TimelineExhausted => LocalLoopbackFault::Simulation,
            Self::ClientSnapshot(_) => LocalLoopbackFault::ClientSnapshot,
            Self::Snapshot(_)
            | Self::Replay(_)
            | Self::InitialSnapshotMismatch { .. }
            | Self::InitialSnapshotContractMismatch(_)
            | Self::StateHashMismatch { .. }
            | Self::ConflictingStateHash { .. } => LocalLoopbackFault::SnapshotMismatch,
            Self::RegressedStateTick { .. } => LocalLoopbackFault::Protocol,
            Self::UnexpectedMessage(_)
            | Self::UnsupportedOwnership
            | Self::InitialSnapshotAfterStart { .. }
            | Self::ConflictingResult { .. } => LocalLoopbackFault::UnexpectedMessage,
            Self::RejectedInput(_) => LocalLoopbackFault::Protocol,
            Self::StartupBudgetExceeded => LocalLoopbackFault::Capacity,
            Self::Config(_) | Self::Endpoint(_) | Self::RuntimeConfig(_) | Self::Failed(_) => {
                LocalLoopbackFault::Runtime
            }
        }
    }
}

impl<SE: fmt::Debug, CE: fmt::Debug> fmt::Display for LocalLoopbackError<SE, CE> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "local loopback match failed: {self:?}")
    }
}

impl<SE: fmt::Debug, CE: fmt::Debug> std::error::Error for LocalLoopbackError<SE, CE> {}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LocalLoopbackMetrics {
    pub network_rounds: u64,
    pub authority_ticks: u64,
    pub local_input_batches: u64,
    pub state_messages: u64,
    pub state_delta_messages: u64,
    pub initial_snapshots_applied: u64,
    pub confirmed_results: u64,
    pub state_history_high_water: u16,
    pub client_stall_authority_ticks: u64,
}

#[derive(Clone, Copy, Debug, Default)]
struct LocalSeatInputHistory {
    frames: [InputFrame; MAX_INPUT_FRAMES_PER_WINDOW],
    len: u8,
}

impl LocalSeatInputHistory {
    fn push(&mut self, frame: InputFrame) -> Result<(), (SimTick, SimTick)> {
        if self.len != 0 {
            let previous = self.frames[0];
            if frame.tick == previous.tick {
                return Err((previous.tick, frame.tick));
            }
            if frame.tick != previous.tick.next()
                || frame.sequence.0 != previous.sequence.0.wrapping_add(1)
            {
                // A local network/render stall may skip ticks. Start a fresh
                // redundancy window rather than manufacturing nonexistent frames.
                self.frames = [InputFrame::default(); MAX_INPUT_FRAMES_PER_WINDOW];
                self.frames[0] = frame;
                self.len = 1;
                return Ok(());
            }
        }
        let old_len = usize::from(self.len).min(MAX_INPUT_FRAMES_PER_WINDOW - 1);
        self.frames.copy_within(0..old_len, 1);
        self.frames[0] = frame;
        self.len = (old_len + 1) as u8;
        Ok(())
    }

    fn window(&self) -> Result<SeatInputWindow, ProtocolValidationError> {
        SeatInputWindow::from_newest_first(&self.frames[..usize::from(self.len)])
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct LocalCommittedInputHistory {
    records: [CommittedInputRecord; MAX_INPUT_FRAMES_PER_WINDOW],
    len: u8,
}

impl LocalCommittedInputHistory {
    fn push(&mut self, record: CommittedInputRecord) {
        let retained = usize::from(self.len).min(MAX_INPUT_FRAMES_PER_WINDOW - 1);
        self.records.copy_within(0..retained, 1);
        self.records[0] = record;
        self.len = (retained + 1) as u8;
    }

    fn window(&self) -> Result<CommittedSeatInputWindow, ProtocolValidationError> {
        CommittedSeatInputWindow::from_newest_first(&self.records[..usize::from(self.len)])
    }

    fn len(&self) -> usize {
        usize::from(self.len)
    }

    fn window_with_len(
        &self,
        len: usize,
    ) -> Result<CommittedSeatInputWindow, ProtocolValidationError> {
        if len == 0 || len > self.len() {
            return Err(ProtocolValidationError::InvalidTickWindow);
        }
        CommittedSeatInputWindow::from_newest_first(&self.records[..len])
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AuthorityTransferStage {
    Begin,
    InputTail,
    Chunks,
    WaitingApplied,
}

struct PendingAuthorityTransfer {
    transfer: AuthorityResyncTransfer,
    stage: AuthorityTransferStage,
    next_chunk: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClientPostSyncStage {
    Applied,
    InitialSync,
    Ready,
}

struct PendingClientPostSync {
    applied: crate::network_protocol::ResyncApplied,
    stage: ClientPostSyncStage,
    initial: bool,
}

#[derive(Clone, Copy, Debug)]
struct StateReceiptHistory {
    slots: [Option<StateHashAndAcks>; LOCAL_STATE_HASH_HISTORY_TICKS],
    len: usize,
    latest_tick: Option<SimTick>,
}

impl Default for StateReceiptHistory {
    fn default() -> Self {
        Self {
            slots: [None; LOCAL_STATE_HASH_HISTORY_TICKS],
            len: 0,
            latest_tick: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StateReceiptError {
    Conflict(StateHash, StateHash),
    Regressed { latest: SimTick },
}

impl StateReceiptHistory {
    const fn slot(tick: SimTick) -> usize {
        tick.0 as usize % LOCAL_STATE_HASH_HISTORY_TICKS
    }

    fn insert(&mut self, message: StateHashAndAcks) -> Result<(), StateReceiptError> {
        if let Some(latest) = self.latest_tick
            && message.authority_tick < latest
        {
            return Err(StateReceiptError::Regressed { latest });
        }
        let slot = Self::slot(message.authority_tick);
        if let Some(previous) = self.slots[slot]
            && previous.authority_tick == message.authority_tick
        {
            if previous.state_hash != message.state_hash {
                return Err(StateReceiptError::Conflict(
                    previous.state_hash,
                    message.state_hash,
                ));
            }
            return Ok(());
        }
        if self.slots[slot].is_none() {
            self.len += 1;
        }
        self.slots[slot] = Some(message);
        self.latest_tick = Some(message.authority_tick);
        Ok(())
    }

    fn get(&self, tick: SimTick) -> Option<&StateHashAndAcks> {
        self.slots[Self::slot(tick)]
            .as_ref()
            .filter(|message| message.authority_tick == tick)
    }
}

/// One local peer connected to one canonical authority. One peer may own one to
/// four seats; every gameplay input still crosses the packet codec/runtime.
pub struct LocalLoopbackMatch<S, C = CanonicalSnapshotMirror>
where
    S: AuthoritySimulation<Snapshot = CanonicalSnapshot>,
    C: InitialSnapshotTarget,
{
    manifest: MatchManifest,
    peer_id: PeerId,
    authority: AuthorityMatch<S>,
    authority_runtime: NetworkRuntime<InProcessEndpoint>,
    client_runtime: NetworkRuntime<InProcessEndpoint>,
    client_world: C,
    assembler: ClientResyncAssembler,
    config: LocalLoopbackConfig,
    network_tick: SimTick,
    input_histories: [LocalSeatInputHistory; MAX_SEATS],
    committed_input_histories: [LocalCommittedInputHistory; MAX_SEATS],
    authority_state_history: AuthorityStateHistory,
    authority_state_sync: AuthorityStateSyncCoordinator,
    authority_transfer: Option<PendingAuthorityTransfer>,
    pending_resync_request: Option<crate::network_protocol::ResyncRequest>,
    pending_post_sync: Option<PendingClientPostSync>,
    pending_state: Option<StateHashAndAcks>,
    pending_delta: Option<StateDeltaAndAcks>,
    pending_committed_inputs: Option<CommittedInputRelay>,
    pending_result: Option<ResultIdentifier>,
    state_history: StateReceiptHistory,
    initial_snapshot: Option<AppliedInitialSync>,
    confirmed_result: Option<ConfirmedSessionResult>,
    queued_result_id: Option<u64>,
    replay_recorder: Option<AuthorityReplayRecorder>,
    completed_replay: Option<Replay>,
    failure: Option<LocalLoopbackFault>,
    metrics: LocalLoopbackMetrics,
    client_pumped_since_authority_step: bool,
    next_transfer_id: u32,
}

impl<S> LocalLoopbackMatch<S, CanonicalSnapshotMirror>
where
    S: AuthoritySimulation<Snapshot = CanonicalSnapshot>,
{
    pub fn new(
        manifest: MatchManifest,
        peer_id: PeerId,
        simulation: S,
        input_config: AuthorityInputConfig,
        config: LocalLoopbackConfig,
    ) -> Result<Self, LocalLoopbackError<S::Error, SnapshotError>> {
        Self::with_client_world(
            manifest,
            peer_id,
            simulation,
            CanonicalSnapshotMirror::default(),
            input_config,
            config,
        )
    }
}

impl<S, C> LocalLoopbackMatch<S, C>
where
    S: AuthoritySimulation<Snapshot = CanonicalSnapshot>,
    C: InitialSnapshotTarget,
{
    pub fn with_client_world(
        manifest: MatchManifest,
        peer_id: PeerId,
        simulation: S,
        mut client_world: C,
        input_config: AuthorityInputConfig,
        config: LocalLoopbackConfig,
    ) -> Result<Self, LocalLoopbackError<S::Error, C::Error>> {
        config.validate().map_err(LocalLoopbackError::Config)?;
        manifest.validate_for_start(SimTick::ZERO)?;
        peer_id.validate()?;
        if manifest.ownership.is_empty()
            || manifest.ownership.as_slice().iter().any(|assignment| {
                !matches!(
                    assignment.owner,
                    SeatOwner::Peer(owner) if owner == peer_id
                ) && assignment.owner != SeatOwner::AuthorityBot
            })
        {
            return Err(LocalLoopbackError::UnsupportedOwnership);
        }
        client_world
            .configure_match(&manifest)
            .map_err(LocalLoopbackError::ClientSnapshot)?;

        let authority = AuthorityMatch::new(manifest, simulation, input_config)
            .map_err(LocalLoopbackError::Authority)?;
        let initial_tick = authority.simulation().current_tick();
        if initial_tick >= manifest.agreed_start_tick {
            return Err(LocalLoopbackError::InitialSnapshotAfterStart {
                snapshot_tick: initial_tick,
                start_tick: manifest.agreed_start_tick,
            });
        }
        let initial_authority_snapshot = authority
            .snapshot_at(initial_tick)
            .expect("AuthorityMatch retains its validated initial snapshot")
            .clone();
        let mut authority_state_history =
            AuthorityStateHistory::new(manifest.match_id, DEFAULT_STATE_SYNC_HISTORY_ENTRIES)?;
        authority_state_history.record_snapshot(&initial_authority_snapshot)?;
        let mut authority_state_sync = AuthorityStateSyncCoordinator::new(manifest.match_id, 1)?;
        authority_state_sync.connect_peer(peer_id)?;
        let replay_recorder = AuthorityReplayRecorder::new(manifest, initial_authority_snapshot)
            .map_err(LocalLoopbackError::Replay)?;

        let mut client_session = ClientSession::new(
            manifest.compatibility,
            SessionTimeouts::default(),
            SimTick::ZERO,
        )?;
        client_session.enter_lobby(SimTick::ZERO)?;
        client_session.start_connecting(SimTick::ZERO)?;
        client_session.transport_connected(SimTick::ZERO)?;
        client_session.authentication_succeeded(peer_id, SimTick::ZERO)?;

        let gate = AuthoritySessionGate::new(manifest)?;
        let (client_endpoint, authority_endpoint) =
            InProcessEndpoint::pair(config.endpoint_capacity_packets)
                .map_err(LocalLoopbackError::Endpoint)?;
        let mut client_runtime = NetworkRuntime::new_client(
            client_endpoint,
            manifest.compatibility,
            client_session,
            config.runtime,
        )
        .map_err(LocalLoopbackError::RuntimeConfig)?;
        let mut authority_runtime = NetworkRuntime::new_authority(
            authority_endpoint,
            manifest.compatibility,
            gate,
            peer_id,
            config.runtime,
        )
        .map_err(LocalLoopbackError::RuntimeConfig)?;

        client_runtime.queue_message(WireMessage::Handshake(Handshake {
            compatibility: manifest.compatibility,
        }))?;
        authority_runtime.queue_start_message(StartMessage::Manifest(manifest))?;

        Ok(Self {
            manifest,
            peer_id,
            authority,
            authority_runtime,
            client_runtime,
            client_world,
            assembler: ClientResyncAssembler::with_default_timeout(manifest.match_id, peer_id)?,
            config,
            network_tick: SimTick::ZERO,
            input_histories: [LocalSeatInputHistory::default(); MAX_SEATS],
            committed_input_histories: [LocalCommittedInputHistory::default(); MAX_SEATS],
            authority_state_history,
            authority_state_sync,
            authority_transfer: None,
            pending_resync_request: None,
            pending_post_sync: None,
            pending_state: None,
            pending_delta: None,
            pending_committed_inputs: None,
            pending_result: None,
            state_history: StateReceiptHistory::default(),
            initial_snapshot: None,
            confirmed_result: None,
            queued_result_id: None,
            replay_recorder: Some(replay_recorder),
            completed_replay: None,
            failure: None,
            metrics: LocalLoopbackMetrics::default(),
            client_pumped_since_authority_step: true,
            next_transfer_id: 1,
        })
    }

    pub const fn manifest(&self) -> &MatchManifest {
        &self.manifest
    }

    pub const fn peer_id(&self) -> PeerId {
        self.peer_id
    }

    pub const fn network_tick(&self) -> SimTick {
        self.network_tick
    }

    pub const fn metrics(&self) -> LocalLoopbackMetrics {
        self.metrics
    }

    pub const fn failure(&self) -> Option<LocalLoopbackFault> {
        self.failure
    }

    pub fn authority(&self) -> &AuthorityMatch<S> {
        &self.authority
    }

    pub fn client_world(&self) -> &C {
        &self.client_world
    }

    pub fn client_world_mut(&mut self) -> &mut C {
        &mut self.client_world
    }

    pub fn client_phase(&self) -> ConnectionPhase {
        self.client_runtime
            .client_session()
            .expect("local client runtime always owns a client session")
            .phase()
    }

    pub const fn initial_snapshot(&self) -> Option<AppliedInitialSync> {
        self.initial_snapshot
    }

    pub const fn confirmed_result(&self) -> Option<ConfirmedSessionResult> {
        self.confirmed_result
    }

    /// Complete authority-accepted replay after the canonical result is known.
    pub const fn completed_replay(&self) -> Option<&Replay> {
        self.completed_replay.as_ref()
    }

    pub fn state_at(&self, tick: SimTick) -> Option<&StateHashAndAcks> {
        self.state_history.get(tick)
    }

    /// Drives bounded packet work without advancing the canonical clock.
    pub fn pump_network_round(&mut self) -> Result<(), LocalLoopbackError<S::Error, C::Error>> {
        self.ensure_active()?;
        let result = (|| {
            self.pump_client_inner()?;
            self.pump_authority_inner()?;
            self.metrics.network_rounds = self.metrics.network_rounds.saturating_add(1);
            Ok(())
        })();
        self.record_failure(result)
    }

    /// Completes handshake, exact manifest agreement, full snapshot application,
    /// readiness, and countdown using bounded network work.
    pub fn start(&mut self) -> Result<(), LocalLoopbackError<S::Error, C::Error>> {
        self.ensure_active()?;
        for _ in 0..self.config.startup_network_round_limit {
            self.pump_network_round()?;
            if self.client_phase() == ConnectionPhase::Countdown {
                break;
            }
        }
        if self.client_phase() != ConnectionPhase::Countdown {
            return self.fail(
                LocalLoopbackFault::Capacity,
                LocalLoopbackError::StartupBudgetExceeded,
            );
        }

        let countdown_start_tick = self
            .client_runtime
            .client_session()
            .and_then(ClientSession::countdown_start_tick)
            .ok_or(LocalLoopbackError::UnexpectedMessage(
                "countdown phase has no authority-selected start boundary",
            ))?;
        let countdown_ticks = countdown_start_tick.0.saturating_sub(self.network_tick.0);
        if countdown_ticks > u64::from(self.config.startup_countdown_tick_limit) {
            return self.fail(
                LocalLoopbackFault::Capacity,
                LocalLoopbackError::StartupBudgetExceeded,
            );
        }
        for _ in 0..countdown_ticks {
            self.advance_network_clock()?;
            self.pump_network_round()?;
        }
        if self.client_phase() != ConnectionPhase::Fighting {
            return self.fail(
                LocalLoopbackFault::Session,
                LocalLoopbackError::UnexpectedPhase {
                    expected: ConnectionPhase::Fighting,
                    actual: self.client_phase(),
                },
            );
        }
        Ok(())
    }

    /// Queues one tick for every locally-owned seat. The message contains the
    /// bounded recent contiguous tail for packet-loss recovery.
    pub fn queue_local_inputs(
        &mut self,
        frames: &[InputFrame],
    ) -> Result<QueueDisposition, LocalLoopbackError<S::Error, C::Error>> {
        self.ensure_active()?;
        let result = self.queue_local_inputs_inner(frames);
        self.record_failure(result)
    }

    /// Pumps the client transport only. Rendering can call this from an
    /// independent networking hook; the authority never depends on render work.
    pub fn pump_client_network(&mut self) -> Result<(), LocalLoopbackError<S::Error, C::Error>> {
        self.ensure_active()?;
        let result = self.pump_client_inner();
        self.record_failure(result)
    }

    /// Pumps the authority transport without stepping gameplay.
    pub fn pump_authority_network(&mut self) -> Result<(), LocalLoopbackError<S::Error, C::Error>> {
        self.ensure_active()?;
        let result = self.pump_authority_inner();
        self.record_failure(result)
    }

    /// Advances exactly one authoritative gameplay tick. Missing input follows
    /// the authority collector's deterministic substitution policy.
    pub fn advance_authority_tick(
        &mut self,
    ) -> Result<AuthorityTickReport, LocalLoopbackError<S::Error, C::Error>> {
        self.ensure_active()?;
        let result = self.advance_authority_tick_inner();
        self.record_failure(result)
    }

    /// Convenience path for normal local play. It still serializes and pumps both
    /// runtime endpoints; it is not a simulation shortcut.
    pub fn run_local_tick(
        &mut self,
        frames: &[InputFrame],
    ) -> Result<AuthorityTickReport, LocalLoopbackError<S::Error, C::Error>> {
        self.queue_local_inputs(frames)?;
        self.pump_client_network()?;
        let report = self.advance_authority_tick()?;
        self.pump_client_network()?;
        self.pump_authority_network()?;
        self.pump_client_network()?;
        Ok(report)
    }

    fn ensure_active(&self) -> Result<(), LocalLoopbackError<S::Error, C::Error>> {
        if let Some(failure) = self.failure {
            Err(LocalLoopbackError::Failed(failure))
        } else {
            Ok(())
        }
    }

    fn record_failure<T>(
        &mut self,
        result: Result<T, LocalLoopbackError<S::Error, C::Error>>,
    ) -> Result<T, LocalLoopbackError<S::Error, C::Error>> {
        if let Err(error) = &result {
            self.failure.get_or_insert(error.fault());
        }
        result
    }

    fn fail<T>(
        &mut self,
        fault: LocalLoopbackFault,
        error: LocalLoopbackError<S::Error, C::Error>,
    ) -> Result<T, LocalLoopbackError<S::Error, C::Error>> {
        self.failure.get_or_insert(fault);
        Err(error)
    }

    fn advance_network_clock(&mut self) -> Result<(), LocalLoopbackError<S::Error, C::Error>> {
        self.network_tick = SimTick(
            self.network_tick
                .0
                .checked_add(1)
                .ok_or(LocalLoopbackError::TimelineExhausted)?,
        );
        Ok(())
    }

    fn queue_local_inputs_inner(
        &mut self,
        frames: &[InputFrame],
    ) -> Result<QueueDisposition, LocalLoopbackError<S::Error, C::Error>> {
        let phase = self.client_phase();
        if phase != ConnectionPhase::Fighting {
            return Err(LocalLoopbackError::UnexpectedPhase {
                expected: ConnectionPhase::Fighting,
                actual: phase,
            });
        }
        let local_seat_count = self
            .manifest
            .ownership
            .as_slice()
            .iter()
            .filter(|assignment| assignment.owner == SeatOwner::Peer(self.peer_id))
            .count();
        if frames.len() != local_seat_count || frames.is_empty() {
            return Err(LocalLoopbackError::InvalidInputSet);
        }
        let expected_tick = self.authority.simulation().current_tick().next();
        let mut seen_mask = 0_u8;
        let mut staged = self.input_histories;
        for frame in frames {
            frame.validate()?;
            if frame.tick != expected_tick {
                return Err(LocalLoopbackError::InvalidInputSet);
            }
            self.manifest
                .ownership
                .validate_peer_input(self.peer_id, frame.seat)?;
            let bit = 1 << frame.seat.get();
            if seen_mask & bit != 0 {
                return Err(LocalLoopbackError::InvalidInputSet);
            }
            seen_mask |= bit;
            if let Err((previous_tick, received_tick)) =
                staged[usize::from(frame.seat.get())].push(*frame)
            {
                return Err(LocalLoopbackError::NonContiguousLocalInput {
                    seat: frame.seat,
                    previous_tick,
                    received_tick,
                });
            }
        }
        for assignment in self.manifest.ownership.as_slice() {
            if assignment.owner == SeatOwner::Peer(self.peer_id)
                && seen_mask & (1 << assignment.seat.get()) == 0
            {
                return Err(LocalLoopbackError::InvalidInputSet);
            }
        }

        let mut windows = [SeatInputWindow::default(); MAX_SEATS];
        let mut window_count = 0_usize;
        for seat_index in 0..MAX_SEATS {
            if seen_mask & (1 << seat_index) == 0 {
                continue;
            }
            windows[window_count] = staged[seat_index].window()?;
            window_count += 1;
        }
        let mut batch = InputBatch::new(
            self.manifest.match_id,
            self.peer_id,
            &windows[..window_count],
        )?;
        if let Some(acknowledgement) = self.client_world.state_baseline_ack() {
            batch = batch.with_state_baseline_ack(acknowledgement)?;
        }
        let disposition = self
            .client_runtime
            .queue_message(WireMessage::InputBatch(batch))?;
        self.input_histories = staged;
        self.metrics.local_input_batches = self.metrics.local_input_batches.saturating_add(1);
        Ok(disposition)
    }

    fn advance_authority_tick_inner(
        &mut self,
    ) -> Result<AuthorityTickReport, LocalLoopbackError<S::Error, C::Error>> {
        if self.queued_result_id.is_some() {
            return Err(LocalLoopbackError::MatchAlreadyFinished);
        }
        let authority_gate = self.authority_runtime.authority_gate();
        let authority_ready = authority_gate.is_some_and(AuthoritySessionGate::all_ready);
        let countdown_start_tick =
            authority_gate.and_then(AuthoritySessionGate::countdown_start_tick);
        if !authority_ready
            || countdown_start_tick.is_none_or(|start_tick| self.network_tick < start_tick)
        {
            return Err(LocalLoopbackError::UnexpectedPhase {
                expected: ConnectionPhase::Fighting,
                actual: self.client_phase(),
            });
        }
        self.advance_network_clock()?;
        self.pump_authority_inner()?;

        let report = self
            .authority
            .step()
            .map_err(LocalLoopbackError::Authority)?;
        let authority_snapshot = self
            .authority
            .snapshot_at(report.tick)
            .expect("AuthorityMatch retains the snapshot captured for its latest report")
            .clone();
        let checkpoint = report.tick.get() % LOCAL_REPLAY_HASH_INTERVAL_TICKS == 0;
        let keyframe = report.tick.get() % LOCAL_REPLAY_KEYFRAME_INTERVAL_TICKS == 0;
        self.replay_recorder
            .as_mut()
            .ok_or(LocalLoopbackError::UnexpectedMessage(
                "authority advanced after replay finalization",
            ))?
            .record_tick(&report, &authority_snapshot, checkpoint, keyframe)
            .map_err(LocalLoopbackError::Replay)?;
        self.authority_state_history
            .record_snapshot(&authority_snapshot)?;
        self.pending_committed_inputs = Some(self.committed_input_message(&report)?);
        let state = self.state_message(&report)?;
        let delta_due = report.tick.get() % u64::from(self.config.state_delta_interval_ticks) == 0
            || report.final_result_id.is_some();
        if delta_due {
            match self.authority_state_sync.build_latest_for_peer(
                &mut self.authority_state_history,
                self.peer_id,
                state.as_slice(),
            )? {
                PeerStateUpdateOutcome::AwaitingBaselineAcknowledgement { .. } => {}
                PeerStateUpdateOutcome::Delta { message, .. } => {
                    self.pending_delta = Some(message);
                }
                PeerStateUpdateOutcome::FullResyncRequired { required, .. } => {
                    if self.authority_transfer.is_none() && self.pending_resync_request.is_none() {
                        self.pending_resync_request = Some(self.assembler.make_request(
                            ResyncReason::HistoryExpired,
                            required.acknowledged.tick,
                            required.acknowledged.hash,
                        ));
                    }
                }
            }
        }
        self.pending_state = Some(state);
        if let Some(result_id) = report.final_result_id {
            match self.queued_result_id {
                Some(previous) if previous != result_id => {
                    return Err(LocalLoopbackError::ConflictingResult {
                        first: previous,
                        second: result_id,
                    });
                }
                Some(_) => {}
                None => {
                    self.queued_result_id = Some(result_id);
                    let recorder = self.replay_recorder.take().ok_or(
                        LocalLoopbackError::UnexpectedMessage(
                            "authority result arrived after replay finalization",
                        ),
                    )?;
                    self.completed_replay = Some(
                        recorder
                            .finish(&authority_snapshot, result_id)
                            .map_err(LocalLoopbackError::Replay)?,
                    );
                    self.pending_result = Some(ResultIdentifier {
                        match_id: self.manifest.match_id,
                        result_id: ResultId::new(result_id)?,
                        final_tick: report.tick,
                        final_state_hash: report.state_hash,
                    });
                }
            }
        }
        self.service_authority_outbound()?;
        // Flush messages queued after the receive/step boundary. This remains
        // nonblocking even when a stalled client fills its bounded endpoint.
        self.pump_authority_inner()?;

        self.metrics.authority_ticks = self.metrics.authority_ticks.saturating_add(1);
        if !self.client_pumped_since_authority_step {
            self.metrics.client_stall_authority_ticks =
                self.metrics.client_stall_authority_ticks.saturating_add(1);
        }
        self.client_pumped_since_authority_step = false;
        Ok(report)
    }

    fn state_message(
        &self,
        report: &AuthorityTickReport,
    ) -> Result<StateHashAndAcks, ProtocolValidationError> {
        let acknowledgement = self.authority.processed_input_acknowledgement();
        let mut acks = [ProcessedInputAck::default(); MAX_SEATS];
        let mut count = 0_usize;
        for seat in acknowledgement.as_slice() {
            let Some(processed) = seat.processed_input else {
                continue;
            };
            acks[count] = ProcessedInputAck {
                seat: seat.seat,
                processed_through: processed.tick,
                sequence: processed.sequence,
            };
            count += 1;
        }
        StateHashAndAcks::new(
            self.manifest.match_id,
            report.tick,
            report.state_hash,
            &acks[..count],
        )
    }

    fn committed_input_message(
        &mut self,
        report: &AuthorityTickReport,
    ) -> Result<CommittedInputRelay, ProtocolValidationError> {
        for record in report.committed_inputs.iter() {
            let source = match record.origin {
                AuthorityInputOrigin::Peer(peer) => CommittedInputSource::Peer(peer),
                AuthorityInputOrigin::AuthorityBot | AuthorityInputOrigin::DisconnectedBot(_) => {
                    CommittedInputSource::AuthorityBot
                }
                AuthorityInputOrigin::MissingSubstitute => CommittedInputSource::MissingSubstitute,
            };
            self.committed_input_histories[usize::from(record.frame.seat.get())].push(
                CommittedInputRecord {
                    frame: record.frame,
                    fighter: record.fighter,
                    source,
                },
            );
        }
        let mut windows = [CommittedSeatInputWindow::default(); MAX_SEATS];
        let mut count = 0;
        for assignment in self.manifest.ownership.as_slice() {
            windows[count] =
                self.committed_input_histories[usize::from(assignment.seat.get())].window()?;
            count += 1;
        }
        CommittedInputRelay::new(self.manifest.match_id, report.tick, &windows[..count])
    }

    fn pump_client_inner(&mut self) -> Result<(), LocalLoopbackError<S::Error, C::Error>> {
        self.client_runtime.pump(self.network_tick);
        if self.client_runtime.connection_state() != RuntimeConnectionState::Active {
            return Err(LocalLoopbackError::TransportDisconnected);
        }
        while let Some(event) = self.client_runtime.try_next_event() {
            self.handle_client_event(event)?;
        }
        let outcome = self
            .client_world
            .poll_authority()
            .map_err(LocalLoopbackError::ClientSnapshot)?;
        self.handle_client_authority_outcome(outcome);
        self.service_client_outbound()?;
        self.client_pumped_since_authority_step = true;
        Ok(())
    }

    fn pump_authority_inner(&mut self) -> Result<(), LocalLoopbackError<S::Error, C::Error>> {
        self.authority_runtime.pump(self.network_tick);
        if self.authority_runtime.connection_state() != RuntimeConnectionState::Active {
            return Err(LocalLoopbackError::TransportDisconnected);
        }
        while let Some(event) = self.authority_runtime.try_next_event() {
            self.handle_authority_event(event)?;
        }
        self.service_authority_outbound()
    }

    fn handle_client_event(
        &mut self,
        event: RuntimeEvent,
    ) -> Result<(), LocalLoopbackError<S::Error, C::Error>> {
        let RuntimeEvent::Message(message) = event else {
            return match event {
                RuntimeEvent::SessionError(error) => Err(LocalLoopbackError::Session(error)),
                RuntimeEvent::TransportDisconnected => {
                    Err(LocalLoopbackError::TransportDisconnected)
                }
                RuntimeEvent::Message(_) => unreachable!(),
            };
        };
        match message {
            WireMessage::Start(StartMessage::Manifest(manifest)) => {
                if manifest != self.manifest {
                    return Err(LocalLoopbackError::UnexpectedMessage(
                        "authority manifest differs from local match",
                    ));
                }
                self.client_runtime
                    .client_session_mut()
                    .expect("local client runtime always owns a session")
                    .content_loaded(self.network_tick)?;
                self.pending_resync_request = Some(self.assembler.make_request(
                    ResyncReason::InitialSync,
                    SimTick::ZERO,
                    StateHash(0),
                ));
            }
            WireMessage::Start(StartMessage::Countdown { .. }) => {}
            WireMessage::ResyncBegin(begin) => {
                match self.assembler.accept_begin(begin, self.network_tick)? {
                    ResyncBeginOutcome::Started | ResyncBeginOutcome::Duplicate => {}
                    ResyncBeginOutcome::Superseded { .. } => {
                        return Err(LocalLoopbackError::UnexpectedMessage(
                            "initial sync was unexpectedly superseded",
                        ));
                    }
                }
                if let Some(completed) = self.assembler.apply_staged_chunks(self.network_tick)? {
                    self.apply_completed_resync(completed)?;
                }
            }
            WireMessage::ResyncChunk(chunk) => {
                if let ResyncChunkOutcome::Complete(completed) =
                    self.assembler.accept_chunk(chunk, self.network_tick)?
                {
                    self.apply_completed_resync(completed)?;
                }
            }
            WireMessage::ResyncInputTail(tail) => {
                if let ResyncInputTailOutcome::Complete(completed) =
                    self.assembler.accept_input_tail(tail, self.network_tick)?
                {
                    self.apply_completed_resync(completed)?;
                }
            }
            WireMessage::CommittedInputRelay(message) => {
                self.validate_committed_input_relay(&message)?;
                let outcome = self
                    .client_world
                    .observe_committed_inputs(&message)
                    .map_err(LocalLoopbackError::ClientSnapshot)?;
                self.handle_client_authority_outcome(outcome);
            }
            WireMessage::StateHashAndAcks(message) => {
                self.accept_state_message(message)?;
                let outcome = self
                    .client_world
                    .observe_authority_hash(&message)
                    .map_err(LocalLoopbackError::ClientSnapshot)?;
                self.handle_client_authority_outcome(outcome);
            }
            WireMessage::StateDeltaAndAcks(message) => {
                self.validate_state_delta(&message)?;
                let outcome = self
                    .client_world
                    .observe_authority_delta(&message)
                    .map_err(LocalLoopbackError::ClientSnapshot)?;
                self.handle_client_authority_outcome(outcome);
                self.metrics.state_delta_messages =
                    self.metrics.state_delta_messages.saturating_add(1);
            }
            WireMessage::ResultIdentifier(result) => self.accept_result(result)?,
            WireMessage::Disconnect(_) => return Err(LocalLoopbackError::TransportDisconnected),
            _ => {
                return Err(LocalLoopbackError::UnexpectedMessage(
                    "message is invalid on the local client lifecycle",
                ));
            }
        }
        Ok(())
    }

    fn apply_completed_resync(
        &mut self,
        completed: crate::resync_transfer::CompletedResyncTransfer,
    ) -> Result<(), LocalLoopbackError<S::Error, C::Error>> {
        let header = &completed.snapshot.header;
        for (matches, field) in [
            (
                header.protocol_version == u32::from(self.manifest.compatibility.protocol.get()),
                "protocol version",
            ),
            (
                header.simulation_version
                    == u32::from(self.manifest.compatibility.simulation.get()),
                "simulation version",
            ),
            (
                header.master_seed == self.manifest.master_gameplay_seed,
                "master gameplay seed",
            ),
        ] {
            if !matches {
                return Err(LocalLoopbackError::InitialSnapshotContractMismatch(field));
            }
        }
        let expected = AppliedCanonicalSnapshot {
            tick: completed.applied.snapshot_tick,
            hash: completed.applied.snapshot_hash,
        };
        let initial = self.initial_snapshot.is_none();
        self.validate_resync_input_tail(&completed.input_tail)?;
        let actual = if initial {
            self.client_world
                .apply_initial_snapshot(&completed.snapshot)
        } else {
            self.client_world.apply_resync_snapshot(&completed.snapshot)
        }
        .map_err(LocalLoopbackError::ClientSnapshot)?;
        if actual != expected {
            return Err(LocalLoopbackError::InitialSnapshotMismatch { expected, actual });
        }
        self.client_world
            .seed_resync_input_tail(&completed.input_tail)
            .map_err(LocalLoopbackError::ClientSnapshot)?;
        if initial {
            self.initial_snapshot = Some(AppliedInitialSync {
                tick: actual.tick,
                hash: actual.hash,
            });
            self.metrics.initial_snapshots_applied =
                self.metrics.initial_snapshots_applied.saturating_add(1);
        }
        self.pending_post_sync = Some(PendingClientPostSync {
            applied: completed.applied,
            stage: ClientPostSyncStage::Applied,
            initial,
        });
        Ok(())
    }

    fn handle_authority_event(
        &mut self,
        event: RuntimeEvent,
    ) -> Result<(), LocalLoopbackError<S::Error, C::Error>> {
        let RuntimeEvent::Message(message) = event else {
            return match event {
                RuntimeEvent::SessionError(error) => Err(LocalLoopbackError::Session(error)),
                RuntimeEvent::TransportDisconnected => {
                    Err(LocalLoopbackError::TransportDisconnected)
                }
                RuntimeEvent::Message(_) => unreachable!(),
            };
        };
        match message {
            WireMessage::Handshake(_) => {}
            WireMessage::Start(
                StartMessage::ManifestAccepted { .. }
                | StartMessage::InitialSyncApplied { .. }
                | StartMessage::Ready { .. },
            ) => {}
            WireMessage::ResyncRequest(request) => self.begin_resync_transfer(request)?,
            WireMessage::ResyncApplied(applied) => {
                let Some(mut pending) = self.authority_transfer.take() else {
                    return Err(LocalLoopbackError::UnexpectedMessage(
                        "resync acknowledgement without an active transfer",
                    ));
                };
                pending.transfer.validate_applied(&applied)?;
                self.authority_state_sync
                    .observe_validated_resync_applied(self.peer_id, &applied)?;
            }
            WireMessage::InputBatch(batch) => {
                if !self
                    .authority_runtime
                    .authority_gate()
                    .is_some_and(AuthoritySessionGate::all_ready)
                {
                    return Err(LocalLoopbackError::UnexpectedMessage(
                        "input arrived before authority readiness",
                    ));
                }
                let report = self.authority.ingest_peer_batch(self.peer_id, &batch)?;
                if report.rejections.invalid != 0
                    || report.rejections.unowned != 0
                    || report.rejections.stale != 0
                    || report.rejections.future != 0
                    || report.rejections.committed_late != 0
                    || report.rejections.sequence != 0
                    || report.rejections.conflicting != 0
                    || report.rejections.capacity != 0
                {
                    return Err(LocalLoopbackError::RejectedInput(report));
                }
                self.authority_state_sync.observe_validated_input_batch(
                    self.peer_id,
                    &batch,
                    &self.authority_state_history,
                )?;
            }
            WireMessage::Disconnect(_) => return Err(LocalLoopbackError::TransportDisconnected),
            _ => {
                return Err(LocalLoopbackError::UnexpectedMessage(
                    "message is invalid on the local authority lifecycle",
                ));
            }
        }
        Ok(())
    }

    fn begin_resync_transfer(
        &mut self,
        request: crate::network_protocol::ResyncRequest,
    ) -> Result<(), LocalLoopbackError<S::Error, C::Error>> {
        if request.match_id != self.manifest.match_id
            || request.peer_id != self.peer_id
            || self.authority_transfer.is_some()
        {
            return Err(LocalLoopbackError::UnexpectedMessage(
                "invalid or duplicate snapshot request",
            ));
        }
        let initial = self.initial_snapshot.is_none();
        if (initial && request.reason != ResyncReason::InitialSync)
            || (!initial && request.reason == ResyncReason::InitialSync)
        {
            return Err(LocalLoopbackError::UnexpectedMessage(
                "snapshot request reason does not match the session lifecycle",
            ));
        }
        if !initial {
            // Anything encoded against the pre-resync baseline becomes invalid
            // once the reliable snapshot is installed. Fresh high-frequency
            // state resumes after the client's new baseline acknowledgement.
            self.pending_delta = None;
            self.pending_state = None;
            self.pending_committed_inputs = None;
        }
        let peer = self
            .authority_runtime
            .authority_gate()
            .and_then(|gate| gate.peer(self.peer_id))
            .ok_or(LocalLoopbackError::UnexpectedMessage(
                "initial sync peer is not in the authority gate",
            ))?;
        if !peer.authenticated || peer.manifest_hash != Some(self.manifest.manifest_hash) {
            return Err(LocalLoopbackError::UnexpectedMessage(
                "initial sync requested before manifest agreement",
            ));
        }
        let tick = self.authority.simulation().current_tick();
        let snapshot =
            self.authority
                .snapshot_at(tick)
                .ok_or(LocalLoopbackError::UnexpectedMessage(
                    "authority initial snapshot is absent",
                ))?;
        let transfer_id = TransferId::new(self.next_transfer_id)?;
        self.next_transfer_id = self
            .next_transfer_id
            .checked_add(1)
            .ok_or(LocalLoopbackError::TimelineExhausted)?;
        let mut windows = [CommittedSeatInputWindow::default(); MAX_SEATS];
        let count = self.manifest.ownership.len();
        if tick == SimTick::ZERO {
            for (index, assignment) in self.manifest.ownership.as_slice().iter().enumerate() {
                windows[index] =
                    CommittedSeatInputWindow::from_newest_first(&[CommittedInputRecord {
                        frame: InputFrame {
                            tick,
                            seat: assignment.seat,
                            ..InputFrame::default()
                        },
                        fighter: assignment.fighter,
                        source: CommittedInputSource::MissingSubstitute,
                    }])?;
            }
        } else {
            let tail_len = self
                .manifest
                .ownership
                .as_slice()
                .iter()
                .map(|assignment| {
                    self.committed_input_histories[usize::from(assignment.seat.get())].len()
                })
                .min()
                .unwrap_or(0)
                .min(MAX_RESYNC_INPUT_TAIL_TICKS);
            if tail_len == 0 {
                return Err(ProtocolValidationError::InvalidTickWindow.into());
            }
            for (index, assignment) in self.manifest.ownership.as_slice().iter().enumerate() {
                windows[index] = self.committed_input_histories[usize::from(assignment.seat.get())]
                    .window_with_len(tail_len)?;
            }
        }
        let transfer = AuthorityResyncTransfer::from_snapshot(
            request,
            transfer_id,
            snapshot,
            &windows[..count],
        )?;
        self.authority_transfer = Some(PendingAuthorityTransfer {
            transfer,
            stage: AuthorityTransferStage::Begin,
            next_chunk: 0,
        });
        Ok(())
    }

    fn accept_state_message(
        &mut self,
        message: StateHashAndAcks,
    ) -> Result<(), LocalLoopbackError<S::Error, C::Error>> {
        if message.match_id != self.manifest.match_id {
            return Err(LocalLoopbackError::Protocol(
                ProtocolValidationError::MatchMismatch,
            ));
        }
        for acknowledgement in message.as_slice() {
            if self
                .manifest
                .ownership
                .assignment_for_seat(acknowledgement.seat)
                .is_none()
            {
                return Err(ProtocolValidationError::UnownedSeat.into());
            }
        }
        if let Some(snapshot) = self.authority.snapshot_at(message.authority_tick) {
            let authority_hash = snapshot.state_hash()?;
            if authority_hash != message.state_hash {
                return Err(LocalLoopbackError::StateHashMismatch {
                    tick: message.authority_tick,
                    authority: authority_hash,
                    received: message.state_hash,
                });
            }
        }
        match self.state_history.insert(message) {
            Ok(()) => {}
            Err(StateReceiptError::Conflict(first, second)) => {
                return Err(LocalLoopbackError::ConflictingStateHash {
                    tick: message.authority_tick,
                    first,
                    second,
                });
            }
            Err(StateReceiptError::Regressed { latest }) => {
                return Err(LocalLoopbackError::RegressedStateTick {
                    latest,
                    received: message.authority_tick,
                });
            }
        }
        self.metrics.state_messages = self.metrics.state_messages.saturating_add(1);
        self.metrics.state_history_high_water = self
            .metrics
            .state_history_high_water
            .max(self.state_history.len as u16);
        Ok(())
    }

    fn validate_state_delta(
        &self,
        message: &StateDeltaAndAcks,
    ) -> Result<(), LocalLoopbackError<S::Error, C::Error>> {
        if message.match_id != self.manifest.match_id {
            return Err(ProtocolValidationError::MatchMismatch.into());
        }
        for acknowledgement in message.as_slice() {
            if self
                .manifest
                .ownership
                .assignment_for_seat(acknowledgement.seat)
                .is_none()
            {
                return Err(ProtocolValidationError::UnownedSeat.into());
            }
        }
        if let Some(snapshot) = self.authority.snapshot_at(message.authority_tick) {
            let authority_hash = snapshot.state_hash()?;
            if authority_hash != message.state_hash {
                return Err(LocalLoopbackError::StateHashMismatch {
                    tick: message.authority_tick,
                    authority: authority_hash,
                    received: message.state_hash,
                });
            }
        }
        Ok(())
    }

    fn validate_committed_input_relay(
        &self,
        message: &CommittedInputRelay,
    ) -> Result<(), LocalLoopbackError<S::Error, C::Error>> {
        if message.match_id != self.manifest.match_id {
            return Err(ProtocolValidationError::MatchMismatch.into());
        }
        if message.len() != self.manifest.ownership.len() {
            return Err(ProtocolValidationError::MissingFighterOwner.into());
        }
        let mut seen = 0_u8;
        for window in message.as_slice() {
            let newest = window
                .newest()
                .ok_or(ProtocolValidationError::EmptyInputWindow)?;
            let assignment = self
                .manifest
                .ownership
                .assignment_for_seat(newest.frame.seat)
                .ok_or(ProtocolValidationError::UnownedSeat)?;
            if assignment.fighter != newest.fighter {
                return Err(ProtocolValidationError::MissingFighterOwner.into());
            }
            for record in window.as_slice() {
                let source_matches = match (assignment.owner, record.source) {
                    (SeatOwner::Peer(expected), CommittedInputSource::Peer(actual)) => {
                        expected == actual
                    }
                    (_, CommittedInputSource::AuthorityBot) => true,
                    (_, CommittedInputSource::MissingSubstitute) => true,
                    _ => false,
                };
                if !source_matches {
                    return Err(ProtocolValidationError::SeatOwnedByDifferentPeer.into());
                }
            }
            seen |= 1 << newest.frame.seat.get();
        }
        for assignment in self.manifest.ownership.as_slice() {
            if seen & (1 << assignment.seat.get()) == 0 {
                return Err(ProtocolValidationError::MissingFighterOwner.into());
            }
        }
        Ok(())
    }

    fn validate_resync_input_tail(
        &self,
        tail: &ResyncInputTail,
    ) -> Result<(), LocalLoopbackError<S::Error, C::Error>> {
        tail.validate()?;
        if tail.match_id != self.manifest.match_id {
            return Err(ProtocolValidationError::MatchMismatch.into());
        }
        if tail.len() != self.manifest.ownership.len() {
            return Err(ProtocolValidationError::MissingFighterOwner.into());
        }
        let mut seen = 0_u8;
        for window in tail.as_slice() {
            let newest = window
                .newest()
                .ok_or(ProtocolValidationError::EmptyInputWindow)?;
            let assignment = self
                .manifest
                .ownership
                .assignment_for_seat(newest.frame.seat)
                .ok_or(ProtocolValidationError::UnownedSeat)?;
            if assignment.fighter != newest.fighter {
                return Err(ProtocolValidationError::MissingFighterOwner.into());
            }
            for record in window.as_slice() {
                let source_matches = match (assignment.owner, record.source) {
                    (SeatOwner::Peer(expected), CommittedInputSource::Peer(actual)) => {
                        expected == actual
                    }
                    (_, CommittedInputSource::AuthorityBot) => true,
                    (_, CommittedInputSource::MissingSubstitute) => true,
                    _ => false,
                };
                if !source_matches {
                    return Err(ProtocolValidationError::SeatOwnedByDifferentPeer.into());
                }
            }
            seen |= 1 << newest.frame.seat.get();
        }
        if self
            .manifest
            .ownership
            .as_slice()
            .iter()
            .any(|assignment| seen & (1 << assignment.seat.get()) == 0)
        {
            return Err(ProtocolValidationError::MissingFighterOwner.into());
        }
        Ok(())
    }

    fn handle_client_authority_outcome(&mut self, outcome: ClientAuthorityOutcome) {
        if let ClientAuthorityOutcome::HardResyncRequired {
            reason,
            last_confirmed_tick,
            last_confirmed_hash,
        } = outcome
            && self.pending_resync_request.is_none()
            && self.authority_transfer.is_none()
        {
            self.pending_resync_request = Some(self.assembler.make_request(
                reason,
                last_confirmed_tick,
                last_confirmed_hash,
            ));
        }
    }

    fn accept_result(
        &mut self,
        result: ResultIdentifier,
    ) -> Result<(), LocalLoopbackError<S::Error, C::Error>> {
        if result.match_id != self.manifest.match_id {
            return Err(LocalLoopbackError::Protocol(
                ProtocolValidationError::MatchMismatch,
            ));
        }
        if let Some(snapshot) = self.authority.snapshot_at(result.final_tick) {
            let authority_hash = snapshot.state_hash()?;
            if authority_hash != result.final_state_hash {
                return Err(LocalLoopbackError::StateHashMismatch {
                    tick: result.final_tick,
                    authority: authority_hash,
                    received: result.final_state_hash,
                });
            }
        }
        if let Some(previous) = self.confirmed_result {
            if previous.result_id != result.result_id.get()
                || previous.final_tick != result.final_tick
                || previous.final_hash != result.final_state_hash
            {
                return Err(LocalLoopbackError::ConflictingResult {
                    first: previous.result_id,
                    second: result.result_id.get(),
                });
            }
            return Ok(());
        }
        let confirmed = self
            .client_runtime
            .client_session()
            .and_then(ClientSession::result)
            .ok_or(LocalLoopbackError::UnexpectedMessage(
                "result event was not accepted by the client session",
            ))?;
        self.confirmed_result = Some(confirmed);
        self.metrics.confirmed_results = self.metrics.confirmed_results.saturating_add(1);
        Ok(())
    }

    fn service_client_outbound(&mut self) -> Result<(), LocalLoopbackError<S::Error, C::Error>> {
        if self.client_runtime.outbound_len() >= self.config.runtime.outbound_capacity {
            return Ok(());
        }
        if let Some(request) = self.pending_resync_request.take() {
            self.client_runtime
                .queue_message(WireMessage::ResyncRequest(request))?;
            return Ok(());
        }

        let Some(mut pending) = self.pending_post_sync.take() else {
            return Ok(());
        };
        match pending.stage {
            ClientPostSyncStage::Applied => {
                self.client_runtime
                    .queue_message(WireMessage::ResyncApplied(pending.applied))?;
                if pending.initial {
                    pending.stage = ClientPostSyncStage::InitialSync;
                    self.pending_post_sync = Some(pending);
                }
            }
            ClientPostSyncStage::InitialSync => {
                let session = self
                    .client_runtime
                    .client_session()
                    .copied()
                    .expect("local client runtime always owns a session");
                let mut preview = session;
                let message = preview.apply_initial_sync(
                    self.manifest.match_id,
                    pending.applied.snapshot_tick,
                    pending.applied.snapshot_hash,
                    self.network_tick,
                )?;
                // The in-process adapter shares one exact monotonic clock. Mark
                // the same readiness evidence represented by three remote probes
                // without manufacturing wall-clock latency in local play.
                preview.mark_clock_synchronized()?;
                for probe in 1..=u32::from(MIN_CLOCK_SYNC_SAMPLES) {
                    self.authority_runtime
                        .authority_gate_mut()
                        .expect("local authority runtime always owns a session gate")
                        .observe_clock_probe(self.peer_id, ClockProbeId::new(probe)?)?;
                }
                self.client_runtime.queue_start_message(message)?;
                *self
                    .client_runtime
                    .client_session_mut()
                    .expect("local client runtime always owns a session") = preview;
                pending.stage = ClientPostSyncStage::Ready;
                self.pending_post_sync = Some(pending);
            }
            ClientPostSyncStage::Ready => {
                let ready = self
                    .client_runtime
                    .client_session()
                    .expect("local client runtime always owns a session")
                    .ready_message()?;
                self.client_runtime.queue_start_message(ready)?;
            }
        }
        Ok(())
    }

    fn service_authority_outbound(&mut self) -> Result<(), LocalLoopbackError<S::Error, C::Error>> {
        if self.authority_runtime.outbound_len() >= self.config.runtime.outbound_capacity {
            return Ok(());
        }

        if let Some(result) = self.pending_result.take() {
            self.authority_runtime
                .queue_message(WireMessage::ResultIdentifier(result))?;
            return Ok(());
        }

        if let Some(pending) = self.authority_transfer.as_mut() {
            match pending.stage {
                AuthorityTransferStage::Begin => {
                    self.authority_runtime
                        .queue_message(WireMessage::ResyncBegin(pending.transfer.begin()))?;
                    pending.stage = AuthorityTransferStage::InputTail;
                    return Ok(());
                }
                AuthorityTransferStage::InputTail => {
                    self.authority_runtime
                        .queue_message(WireMessage::ResyncInputTail(
                            pending.transfer.input_tail(),
                        ))?;
                    pending.stage = AuthorityTransferStage::Chunks;
                    return Ok(());
                }
                AuthorityTransferStage::Chunks => {
                    let chunk = pending
                        .transfer
                        .chunks_from(pending.next_chunk)?
                        .next()
                        .ok_or(LocalLoopbackError::UnexpectedMessage(
                            "resync iterator ended before declared chunk count",
                        ))?;
                    self.authority_runtime
                        .queue_message(WireMessage::ResyncChunk(chunk))?;
                    pending.next_chunk += 1;
                    if pending.next_chunk == pending.transfer.begin().chunk_count {
                        pending.stage = AuthorityTransferStage::WaitingApplied;
                    }
                    return Ok(());
                }
                AuthorityTransferStage::WaitingApplied => {}
            }
        }

        if let Some(inputs) = self.pending_committed_inputs.take() {
            self.authority_runtime
                .queue_message(WireMessage::CommittedInputRelay(inputs))?;
            return Ok(());
        }

        if let Some(state) = self.pending_state.take() {
            self.authority_runtime
                .queue_message(WireMessage::StateHashAndAcks(state))?;
            return Ok(());
        }
        if let Some(delta) = self.pending_delta.take() {
            self.authority_runtime
                .queue_message(WireMessage::StateDeltaAndAcks(delta))?;
        }
        Ok(())
    }
}

impl<SE, CE> From<ProtocolValidationError> for LocalLoopbackError<SE, CE> {
    fn from(error: ProtocolValidationError) -> Self {
        Self::Protocol(error)
    }
}

impl<SE, CE> From<SessionError> for LocalLoopbackError<SE, CE> {
    fn from(error: SessionError) -> Self {
        Self::Session(error)
    }
}

impl<SE, CE> From<RuntimeQueueError> for LocalLoopbackError<SE, CE> {
    fn from(error: RuntimeQueueError) -> Self {
        Self::RuntimeQueue(error)
    }
}

impl<SE, CE> From<ResyncTransferError> for LocalLoopbackError<SE, CE> {
    fn from(error: ResyncTransferError) -> Self {
        Self::Resync(error)
    }
}

impl<SE, CE> From<StateSyncError> for LocalLoopbackError<SE, CE> {
    fn from(error: StateSyncError) -> Self {
        Self::StateSync(error)
    }
}

impl<SE, CE> From<PeerStateSyncError> for LocalLoopbackError<SE, CE> {
    fn from(error: PeerStateSyncError) -> Self {
        Self::PeerStateSync(error)
    }
}

impl<SE, CE> From<SnapshotError> for LocalLoopbackError<SE, CE> {
    fn from(error: SnapshotError) -> Self {
        Self::Snapshot(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::authority_input::CommittedTickInputs;
    use crate::determinism::{FighterId, SimEntityKind};
    use crate::network_protocol::{
        AuthorityKind, BuildId, CompatibilityId, DefinitionId, FighterSlotConfig,
        GameplayContentHash, InputButtons, InputSequence, ManifestHash, MatchId, ProtocolVersion,
        QuantizedAxis, ReplayFormatVersion, SIMULATION_HZ, SeatAssignment, SeatOwnership,
        SimulationVersion, TeamId,
    };
    use crate::snapshot::{
        ArenaRuntimeSnapshot, FighterSnapshot, MatchPhaseSnapshot, MatchResultSnapshot,
        MatchStateSnapshot, MatchStatsSnapshot, PoolAllocatorSnapshot, SnapshotHeader,
    };

    const MATCH_BYTES: [u8; 16] = *b"loopback-test-01";

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum ToyError {
        TickGap,
        MissingSeat,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum ToyRollbackError {
        TickGap,
        MissingSeat,
        Snapshot(SnapshotError),
    }

    struct ToySimulation {
        snapshot: CanonicalSnapshot,
        finish_tick: Option<SimTick>,
        bot_seats_mask: u8,
    }

    impl ToySimulation {
        fn new(finish_tick: Option<u64>) -> Self {
            let allocators = SimEntityKind::ALL
                .into_iter()
                .map(|kind| PoolAllocatorSnapshot::empty(kind, 1).unwrap())
                .collect();
            let fighters = FighterId::ALL.map(|fighter| FighterSnapshot {
                occupied: true,
                active: true,
                ..FighterSnapshot::empty(fighter)
            });
            Self {
                snapshot: CanonicalSnapshot {
                    header: SnapshotHeader::new(1, 1, 0xAFC0, MATCH_BYTES, SimTick::ZERO, 77),
                    match_state: MatchStateSnapshot {
                        phase: MatchPhaseSnapshot::Fight,
                        active_slots_mask: 0b1111,
                        stocks: [3; MAX_SEATS],
                        ..MatchStateSnapshot::default()
                    },
                    fighters,
                    arena: ArenaRuntimeSnapshot::default(),
                    allocators,
                    dynamic_objects: Vec::new(),
                    rng_streams: Vec::new(),
                    stats: MatchStatsSnapshot::default(),
                },
                finish_tick: finish_tick.map(SimTick),
                bot_seats_mask: 0,
            }
        }

        fn with_bot_seats(mut self, mask: u8) -> Self {
            self.bot_seats_mask = mask;
            self
        }
    }

    impl AuthoritySimulation for ToySimulation {
        type Snapshot = CanonicalSnapshot;
        type Error = ToyError;

        fn current_tick(&self) -> SimTick {
            self.snapshot.header.tick
        }

        fn step(&mut self, inputs: &CommittedTickInputs) -> Result<(), Self::Error> {
            if inputs.tick != self.snapshot.header.tick.next() {
                return Err(ToyError::TickGap);
            }
            if inputs.len() != MAX_SEATS {
                return Err(ToyError::MissingSeat);
            }
            self.snapshot.header.tick = inputs.tick;
            self.snapshot.stats.gameplay_ticks = inputs.tick.get();
            for record in inputs.iter() {
                let index = record.fighter.index();
                let input_value = i32::from(record.frame.movement_x.get()) + 128;
                self.snapshot.stats.damage_by_fighter[index] =
                    self.snapshot.stats.damage_by_fighter[index]
                        .wrapping_add(input_value)
                        .wrapping_add(i32::from(record.frame.held_buttons.bits()));
            }
            if self
                .finish_tick
                .is_some_and(|finish| self.snapshot.header.tick >= finish)
            {
                self.snapshot.match_state.phase = MatchPhaseSnapshot::Result;
                self.snapshot.match_state.result = MatchResultSnapshot::Draw {
                    decided_tick: inputs.tick,
                };
            }
            Ok(())
        }

        fn capture_snapshot(&self) -> Result<Self::Snapshot, Self::Error> {
            Ok(self.snapshot.clone())
        }

        fn generate_authority_bot_frames(
            &mut self,
            tick: SimTick,
        ) -> Result<Option<[Option<InputFrame>; MAX_SEATS]>, Self::Error> {
            if self.bot_seats_mask == 0 {
                return Ok(None);
            }
            Ok(Some(std::array::from_fn(|seat| {
                (self.bot_seats_mask & (1 << seat) != 0).then(|| InputFrame {
                    tick,
                    seat: SeatId::new(seat as u8).unwrap(),
                    movement_x: QuantizedAxis::new(-40 + seat as i8).unwrap(),
                    sequence: InputSequence(tick.get() as u16),
                    ..InputFrame::default()
                })
            })))
        }

        fn final_result_id(&self) -> Option<u64> {
            self.finish_tick
                .is_some_and(|finish| self.snapshot.header.tick >= finish)
                .then_some(0xAFC0_0000_0000_0042)
        }
    }

    struct ToyRollbackWorld {
        snapshot: CanonicalSnapshot,
    }

    impl ToyRollbackWorld {
        fn new() -> Self {
            Self {
                snapshot: ToySimulation::new(None).snapshot,
            }
        }
    }

    impl RollbackWorld for ToyRollbackWorld {
        type Snapshot = CanonicalSnapshot;
        type Error = ToyRollbackError;

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
                return Err(ToyRollbackError::TickGap);
            }
            if inputs.len() != MAX_SEATS {
                return Err(ToyRollbackError::MissingSeat);
            }
            self.snapshot.header.tick = tick;
            self.snapshot.stats.gameplay_ticks = tick.get();
            for (index, frame) in inputs.iter().enumerate() {
                self.snapshot.stats.damage_by_fighter[index] =
                    self.snapshot.stats.damage_by_fighter[index]
                        .wrapping_add(i32::from(frame.movement_x.get()) + 128)
                        .wrapping_add(i32::from(frame.held_buttons.bits()));
            }
            Ok(())
        }

        fn state_hash(&self) -> Result<u64, Self::Error> {
            self.snapshot
                .canonical_hash()
                .map_err(ToyRollbackError::Snapshot)
        }
    }

    fn peer_id() -> PeerId {
        PeerId::new(7).unwrap()
    }

    fn compatibility() -> CompatibilityId {
        CompatibilityId {
            protocol: ProtocolVersion::new(1).unwrap(),
            simulation: SimulationVersion::new(1).unwrap(),
            replay: ReplayFormatVersion::new(1).unwrap(),
            build: BuildId::new([1; 16]).unwrap(),
            gameplay_content: GameplayContentHash::new([2; 32]).unwrap(),
        }
    }

    fn manifest() -> MatchManifest {
        let assignments = std::array::from_fn::<_, MAX_SEATS, _>(|index| SeatAssignment {
            seat: SeatId::new(index as u8).unwrap(),
            fighter: FighterId::new(index as u8).unwrap(),
            owner: SeatOwner::Peer(peer_id()),
        });
        let ownership = SeatOwnership::from_assignments(&assignments).unwrap();
        let slots = std::array::from_fn(|index| FighterSlotConfig {
            occupied: true,
            fighter: FighterId::new(index as u8).unwrap(),
            team: TeamId::new(index as u8).unwrap(),
            character: DefinitionId::new(index as u16 + 1).unwrap(),
            style: DefinitionId::new(1).unwrap(),
            equipment: DefinitionId::new(0).unwrap(),
        });
        MatchManifest {
            compatibility: compatibility(),
            manifest_hash: ManifestHash(0x4455),
            match_id: MatchId::new(MATCH_BYTES).unwrap(),
            authority: AuthorityKind::Offline,
            trusted_results: false,
            arena: DefinitionId::new(1).unwrap(),
            rules: DefinitionId::new(1).unwrap(),
            slots,
            ownership,
            master_gameplay_seed: 77,
            rng_scheme_version: 1,
            tick_rate_hz: SIMULATION_HZ,
            input_delay_ticks: 2,
            rollback_limit_ticks: 12,
            snapshot_history_ticks: 32,
            agreed_start_tick: SimTick(10),
        }
    }

    fn frames(tick: u64) -> [InputFrame; MAX_SEATS] {
        std::array::from_fn(|seat| InputFrame {
            tick: SimTick(tick),
            seat: SeatId::new(seat as u8).unwrap(),
            movement_x: QuantizedAxis::new((seat as i8 + 1) * 10).unwrap(),
            movement_y: QuantizedAxis::new(-(seat as i8)).unwrap(),
            held_buttons: InputButtons::new(InputButtons::LIGHT).unwrap(),
            pressed_buttons: InputButtons::new(InputButtons::LIGHT).unwrap(),
            released_buttons: InputButtons::default(),
            sequence: InputSequence(tick as u16),
        })
    }

    fn mixed_manifest() -> MatchManifest {
        let mut value = manifest();
        let assignments = std::array::from_fn::<_, MAX_SEATS, _>(|index| SeatAssignment {
            seat: SeatId::new(index as u8).unwrap(),
            fighter: FighterId::new(index as u8).unwrap(),
            owner: if index < 2 {
                SeatOwner::Peer(peer_id())
            } else {
                SeatOwner::AuthorityBot
            },
        });
        value.ownership = SeatOwnership::from_assignments(&assignments).unwrap();
        value
    }

    fn runner(
        finish_tick: Option<u64>,
    ) -> LocalLoopbackMatch<ToySimulation, CanonicalSnapshotMirror> {
        LocalLoopbackMatch::new(
            manifest(),
            peer_id(),
            ToySimulation::new(finish_tick),
            AuthorityInputConfig::default(),
            LocalLoopbackConfig::default(),
        )
        .unwrap()
    }

    #[test]
    fn four_local_seats_complete_full_startup_and_multiple_wire_ticks() {
        let mut runner = runner(None);
        runner.start().unwrap();
        assert_eq!(runner.client_phase(), ConnectionPhase::Fighting);

        let initial = runner.initial_snapshot().unwrap();
        let authority_initial = runner.authority().snapshot_at(SimTick::ZERO).unwrap();
        assert_eq!(initial.tick, SimTick::ZERO);
        assert_eq!(initial.hash, authority_initial.state_hash().unwrap());
        assert_eq!(runner.client_world().snapshot().unwrap(), authority_initial);

        for tick in 1..=8 {
            let report = runner.run_local_tick(&frames(tick)).unwrap();
            assert_eq!(report.tick, SimTick(tick));
            assert_eq!(report.substituted_inputs, 0);
            let received = runner.state_at(SimTick(tick)).unwrap();
            assert_eq!(received.state_hash, report.state_hash);
            assert_eq!(received.len(), MAX_SEATS);
            for acknowledgement in received.as_slice() {
                assert_eq!(acknowledgement.processed_through, SimTick(tick));
                assert_eq!(acknowledgement.sequence, InputSequence(tick as u16));
            }
        }
        assert_eq!(runner.metrics().local_input_batches, 8);
        assert_eq!(runner.metrics().state_messages, 8);
        assert_eq!(runner.metrics().state_delta_messages, 2);
    }

    #[test]
    fn stalled_client_does_not_stop_or_unbound_authority_stepping() {
        let mut config = LocalLoopbackConfig::default();
        config.endpoint_capacity_packets = 4;
        let mut runner = LocalLoopbackMatch::new(
            manifest(),
            peer_id(),
            ToySimulation::new(None),
            AuthorityInputConfig::default(),
            config,
        )
        .unwrap();
        runner.start().unwrap();

        for tick in 1..=40 {
            let report = runner.advance_authority_tick().unwrap();
            assert_eq!(report.tick, SimTick(tick));
            assert_eq!(report.substituted_inputs, MAX_SEATS as u8);
        }
        assert_eq!(runner.authority().simulation().current_tick(), SimTick(40));
        assert_eq!(runner.metrics().authority_ticks, 40);
        assert_eq!(runner.metrics().client_stall_authority_ticks, 39);
        assert!(runner.authority_runtime.outbound_len() <= config.runtime.outbound_capacity);
    }

    #[test]
    fn mixed_local_humans_and_authority_bots_commit_and_relay_all_seats() {
        let mut runner = LocalLoopbackMatch::new(
            mixed_manifest(),
            peer_id(),
            ToySimulation::new(None).with_bot_seats(0b1100),
            AuthorityInputConfig::default(),
            LocalLoopbackConfig::default(),
        )
        .unwrap();
        runner.start().unwrap();

        let local = frames(1);
        let report = runner.run_local_tick(&local[..2]).unwrap();
        assert_eq!(report.committed_inputs.len(), MAX_SEATS);
        for seat in 0..2 {
            assert!(matches!(
                report.committed_inputs.by_seat[seat].unwrap().origin,
                AuthorityInputOrigin::Peer(owner) if owner == peer_id()
            ));
        }
        for seat in 2..MAX_SEATS {
            assert_eq!(
                report.committed_inputs.by_seat[seat].unwrap().origin,
                AuthorityInputOrigin::AuthorityBot
            );
        }
        // Processed-input acknowledgements exist only for peer-authored frames;
        // authority bots have no sender-side input history to acknowledge. Their
        // canonical frames still travel in the committed-input relay, whose
        // production validator requires one window for every occupied seat.
        let state = runner.state_at(SimTick(1)).unwrap();
        assert_eq!(state.len(), 2);
        for (seat, acknowledgement) in state.as_slice().iter().enumerate() {
            assert_eq!(acknowledgement.seat, SeatId::new(seat as u8).unwrap());
            assert_eq!(acknowledgement.processed_through, SimTick(1));
        }
        for (seat, history) in runner.committed_input_histories.iter().enumerate() {
            assert_eq!(history.len(), 1);
            let window = history.window().unwrap();
            let newest = window.newest().unwrap();
            assert_eq!(newest.frame.seat, SeatId::new(seat as u8).unwrap());
            assert_eq!(
                newest.source,
                if seat < 2 {
                    CommittedInputSource::Peer(peer_id())
                } else {
                    CommittedInputSource::AuthorityBot
                }
            );
        }
    }

    #[test]
    fn confirmed_result_is_delivered_exactly_once_even_when_retransmitted() {
        let mut runner = runner(Some(2));
        runner.start().unwrap();
        runner.run_local_tick(&frames(1)).unwrap();
        let final_report = runner.run_local_tick(&frames(2)).unwrap();
        let confirmed = runner.confirmed_result().unwrap();
        assert_eq!(confirmed.result_id, 0xAFC0_0000_0000_0042);
        assert_eq!(confirmed.final_tick, final_report.tick);
        assert_eq!(confirmed.final_hash, final_report.state_hash);
        assert_eq!(runner.metrics().confirmed_results, 1);
        let replay = runner.completed_replay().unwrap();
        replay.validate().unwrap();
        assert_eq!(replay.inputs.len(), 2);
        assert_eq!(replay.final_result.confirmed_tick, final_report.tick);
        assert_eq!(replay.final_result.state_hash, final_report.state_hash);
        assert!(replay.inputs.iter().all(|tick| {
            tick.fighters
                .iter()
                .all(|input| input.source == crate::replay::ReplayInputSource::Peer)
        }));

        runner
            .authority_runtime
            .queue_message(WireMessage::ResultIdentifier(ResultIdentifier {
                match_id: runner.manifest.match_id,
                result_id: ResultId::new(confirmed.result_id).unwrap(),
                final_tick: confirmed.final_tick,
                final_state_hash: confirmed.final_hash,
            }))
            .unwrap();
        runner.pump_authority_network().unwrap();
        runner.pump_client_network().unwrap();
        runner.pump_authority_network().unwrap();
        assert_eq!(runner.metrics().confirmed_results, 1);
        assert_eq!(runner.client_runtime.metrics().duplicate_results, 1);
    }

    #[test]
    fn predicted_loopback_finishes_with_authority_hash_and_result_parity() {
        let predicted = crate::predicted_client::PredictedClient::new(
            ToyRollbackWorld::new(),
            manifest().match_id,
            32,
        )
        .unwrap();
        let mut runner = LocalLoopbackMatch::with_client_world(
            manifest(),
            peer_id(),
            ToySimulation::new(Some(3)),
            predicted,
            AuthorityInputConfig::default(),
            LocalLoopbackConfig::default(),
        )
        .unwrap();
        runner.start().unwrap();

        let mut final_report = None;
        for tick in 1..=3 {
            let inputs = frames(tick);
            runner
                .client_world_mut()
                .predict_next(inputs.map(Some))
                .unwrap();
            final_report = Some(runner.run_local_tick(&inputs).unwrap());
        }
        for _ in 0..8 {
            runner.pump_network_round().unwrap();
        }

        let final_report = final_report.unwrap();
        assert_eq!(
            runner.client_world().world().state_hash().unwrap(),
            final_report.state_hash.0
        );
        let result = runner.confirmed_result().unwrap();
        assert_eq!(result.final_tick, final_report.tick);
        assert_eq!(result.final_hash, final_report.state_hash);
        assert_eq!(
            runner.client_world().confirmed_tick(),
            Some(final_report.tick)
        );
    }

    #[test]
    fn predicted_loopback_applies_reliable_hard_resync_beyond_rollback_limit() {
        let predicted = crate::predicted_client::PredictedClient::new(
            ToyRollbackWorld::new(),
            manifest().match_id,
            32,
        )
        .unwrap();
        let mut runner = LocalLoopbackMatch::with_client_world(
            manifest(),
            peer_id(),
            ToySimulation::new(None),
            predicted,
            AuthorityInputConfig::default(),
            LocalLoopbackConfig::default(),
        )
        .unwrap();
        runner.start().unwrap();
        for tick in 1..=14 {
            runner
                .client_world_mut()
                .predict_next(frames(tick).map(Some))
                .unwrap();
        }

        let mut authority_inputs = frames(1);
        authority_inputs[0].movement_x = QuantizedAxis::new(-80).unwrap();
        let report = runner.run_local_tick(&authority_inputs).unwrap();
        for _ in 0..32 {
            runner.pump_network_round().unwrap();
        }

        assert_eq!(runner.client_phase(), ConnectionPhase::Fighting);
        assert_eq!(runner.client_world().predicted_tick(), Some(report.tick));
        assert_eq!(
            runner.client_world().world().state_hash().unwrap(),
            report.state_hash.0
        );
        assert_eq!(runner.client_world().metrics().hard_resync_requests, 1);
        assert_eq!(runner.client_world().metrics().hard_resyncs_applied, 1);
        assert!(runner.authority_transfer.is_none());
    }

    #[test]
    fn wrong_peer_input_fails_closed_and_cannot_be_stepped_afterward() {
        let mut runner = runner(None);
        runner.start().unwrap();
        let window = SeatInputWindow::from_newest_first(&[frames(1)[0]]).unwrap();
        let wrong_peer = PeerId::new(999).unwrap();
        let hostile = InputBatch::new(runner.manifest.match_id, wrong_peer, &[window]).unwrap();
        runner
            .client_runtime
            .queue_message(WireMessage::InputBatch(hostile))
            .unwrap();
        runner.pump_client_network().unwrap();
        assert!(matches!(
            runner.pump_authority_network(),
            Err(LocalLoopbackError::Protocol(
                ProtocolValidationError::PeerMismatch
            ))
        ));
        assert_eq!(runner.failure(), Some(LocalLoopbackFault::Protocol));
        assert!(matches!(
            runner.advance_authority_tick(),
            Err(LocalLoopbackError::Failed(LocalLoopbackFault::Protocol))
        ));
        assert_eq!(
            runner.authority().simulation().current_tick(),
            SimTick::ZERO
        );
    }

    #[test]
    fn invalid_capacity_and_multi_peer_ownership_are_rejected_before_io() {
        let mut zero_capacity = LocalLoopbackConfig::default();
        zero_capacity.endpoint_capacity_packets = 0;
        assert!(matches!(
            LocalLoopbackMatch::new(
                manifest(),
                peer_id(),
                ToySimulation::new(None),
                AuthorityInputConfig::default(),
                zero_capacity,
            ),
            Err(LocalLoopbackError::Config(
                LocalLoopbackConfigError::EndpointCapacity
            ))
        ));

        let mut invalid_delta_cadence = LocalLoopbackConfig::default();
        invalid_delta_cadence.state_delta_interval_ticks = 0;
        assert!(matches!(
            LocalLoopbackMatch::new(
                manifest(),
                peer_id(),
                ToySimulation::new(None),
                AuthorityInputConfig::default(),
                invalid_delta_cadence,
            ),
            Err(LocalLoopbackError::Config(
                LocalLoopbackConfigError::StateDeltaInterval
            ))
        ));

        let mut hostile_manifest = manifest();
        let mut assignments = hostile_manifest.ownership.as_slice().to_vec();
        assignments[3].owner = SeatOwner::Peer(PeerId::new(55).unwrap());
        hostile_manifest.ownership = SeatOwnership::from_assignments(&assignments).unwrap();
        assert!(matches!(
            LocalLoopbackMatch::new(
                hostile_manifest,
                peer_id(),
                ToySimulation::new(None),
                AuthorityInputConfig::default(),
                LocalLoopbackConfig::default(),
            ),
            Err(LocalLoopbackError::UnsupportedOwnership)
        ));
    }
}
