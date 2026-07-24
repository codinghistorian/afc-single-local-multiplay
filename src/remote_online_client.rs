//! Threaded production client for a remote AFC authority.
//!
//! The render thread never owns a socket or a rollback world. A dedicated
//! 60 Hz worker owns both the transport-independent client protocol and its
//! predicted [`LiveSimulationDriver`]. Local input crosses a bounded,
//! nonblocking queue; rendering observes a latest-wins canonical snapshot plus
//! bounded rollback-aware presentation sidecars. Confirmed results and terminal
//! failures use reliable retained slots.

use bevy::prelude::World;
use core::fmt;
use std::cell::Cell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
#[cfg(test)]
use std::time::Duration;
use std::time::Instant;

use crate::arena::{ArenaPresentationIntent, ArenaPresentationIntentJournal};
use crate::authority_thread::{AUTHORITY_THREAD_TICK_RATE_HZ, SixtyHzSchedule};
use crate::bee_skills::{BeePresentationIntent, BeePresentationIntentJournal};
use crate::chick_skills::{ChickPresentationIntent, ChickPresentationIntentJournal};
use crate::client_protocol::{
    ClientProtocolBuildError, ClientProtocolConfig, ClientProtocolError, ClientProtocolFatalError,
    ClientProtocolFault, ClientProtocolMetrics, ClientProtocolRecoverableError, ClientProtocolTime,
    RemotePredictedClientProtocol,
};
use crate::combat::{
    CombatPresentationCueIntent, CombatPresentationIntent, CombatPresentationIntentJournal,
};
use crate::confirmed_progression::{ConfirmedProgressionError, ConfirmedProgressionLedger};
use crate::fighter::{FighterPresentationIntent, FighterPresentationIntentJournal};
use crate::headless::{HeadlessBuildError, HeadlessMatchConfig, build_predicted_simulation};
use crate::items::{ItemPresentationIntent, ItemPresentationIntentJournal};
use crate::live_authority::{LiveSimulationDriver, LiveSimulationError};
use crate::live_input::local_tick_to_network_input;
use crate::match_presentation::ConfirmedMatchPresentation;
use crate::network_io::NonBlockingDatagramEndpoint;
use crate::network_protocol::{
    ConnectionPhase, DisconnectMessage, InputButtons, InputFrame, InputSequence, MAX_SEATS,
    MatchManifest, PeerId, QuantizedAxis, SeatId, SeatOwner, SimTick,
};
use crate::network_quality::{
    NetworkQualityError, NetworkQualityMonitor, NetworkQualityPolicy, NetworkQualitySample,
    NetworkQualitySnapshot,
};
use crate::network_runtime::{RuntimeConnectionState, RuntimeMetrics};
use crate::online_failure::{
    OnlineFailure, OnlineFailureCode, OnlineFailureSeverity, OnlineRecoveryAction,
};
use crate::penguin_skills::{PenguinPresentationIntent, PenguinPresentationIntentJournal};
use crate::predicted_client::PredictedClient;
use crate::presentation_projection::{
    LivePresentationProjectionError, LivePresentationProjectionReport, LivePresentationProjector,
};
use crate::rollback::{
    HardResyncReason, InstantRollbackTiming, RollbackEventDiscard, RollbackMetrics,
};
use crate::session::ConfirmedSessionResult;
use crate::sim_event::{
    EventEmitError, MAX_SIM_EVENTS_PER_TICK, SIM_EVENT_HISTORY_TICKS, SimEvent, SimEventJournal,
    TickEventBuffer,
};
use crate::snapshot::CanonicalSnapshot;
use crate::specials::{SpecialPresentationIntent, SpecialPresentationIntentJournal};
use crate::tick_input::{LocalSeatId, LocalTickInputState};

pub const DEFAULT_REMOTE_CLIENT_COMMAND_CAPACITY: usize = 64;
pub const MAX_REMOTE_CLIENT_COMMAND_CAPACITY: usize = 1_024;
pub const DEFAULT_REMOTE_CLIENT_COMMANDS_PER_SERVICE: usize = 64;
pub const MAX_REMOTE_CLIENT_COMMANDS_PER_SERVICE: usize = 1_024;

const REMOTE_CLIENT_SIGNAL_CAPACITY: usize = 1;
#[cfg(test)]
const MICROS_PER_SECOND: u64 = 1_000_000;

type LiveRemoteProtocol<E> = RemotePredictedClientProtocol<
    E,
    LiveSimulationDriver,
    WorkerRollbackHooks,
    InstantRollbackTiming,
>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemoteOnlineClientConfig {
    pub protocol: ClientProtocolConfig,
    pub quality_policy: NetworkQualityPolicy,
    pub command_capacity: usize,
    pub max_commands_per_service: usize,
}

impl Default for RemoteOnlineClientConfig {
    fn default() -> Self {
        Self {
            protocol: ClientProtocolConfig::default(),
            quality_policy: NetworkQualityPolicy::default(),
            command_capacity: DEFAULT_REMOTE_CLIENT_COMMAND_CAPACITY,
            max_commands_per_service: DEFAULT_REMOTE_CLIENT_COMMANDS_PER_SERVICE,
        }
    }
}

impl RemoteOnlineClientConfig {
    pub fn validate(self) -> Result<(), RemoteOnlineClientConfigError> {
        self.protocol
            .validate()
            .map_err(RemoteOnlineClientConfigError::Protocol)?;
        self.quality_policy
            .validate()
            .map_err(RemoteOnlineClientConfigError::Quality)?;
        if self.command_capacity == 0 || self.command_capacity > MAX_REMOTE_CLIENT_COMMAND_CAPACITY
        {
            return Err(RemoteOnlineClientConfigError::CommandCapacity);
        }
        if self.max_commands_per_service == 0
            || self.max_commands_per_service > MAX_REMOTE_CLIENT_COMMANDS_PER_SERVICE
        {
            return Err(RemoteOnlineClientConfigError::CommandServiceLimit);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteOnlineClientConfigError {
    Protocol(ClientProtocolBuildError),
    Quality(NetworkQualityError),
    CommandCapacity,
    CommandServiceLimit,
}

impl fmt::Display for RemoteOnlineClientConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid remote online client configuration: {self:?}"
        )
    }
}

impl std::error::Error for RemoteOnlineClientConfigError {}

#[derive(Debug)]
pub enum RemoteOnlineClientStartError {
    InvalidConfig(RemoteOnlineClientConfigError),
    InvalidMatch(HeadlessBuildError),
    PeerOwnsNoSeat,
    ReconnectBoundaryUnavailable,
    GenerationExhausted,
    TickRateMismatch { manifest_hz: u16 },
    Spawn(std::io::Error),
}

impl fmt::Display for RemoteOnlineClientStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "remote online client could not start: {self:?}")
    }
}

impl std::error::Error for RemoteOnlineClientStartError {}

#[derive(Debug)]
pub enum RemoteOnlinePresentationError {
    Projection(LivePresentationProjectionError),
    Progression(ConfirmedProgressionError),
    Event(EventEmitError),
    EventIdentityChanged,
}

impl fmt::Display for RemoteOnlinePresentationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "remote online presentation failed: {self:?}")
    }
}

impl std::error::Error for RemoteOnlinePresentationError {}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RemoteOnlineClientPhase {
    #[default]
    Connecting,
    Loading,
    Synchronizing,
    Ready,
    Countdown,
    Fighting,
    ConfirmingResult,
    Results,
    Reconnecting,
    Stopped,
    Failed,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RemoteOnlineWorkerMetrics {
    pub command_queue_depth: usize,
    pub command_queue_high_water: usize,
    pub input_commands_submitted: u64,
    pub quality_commands_submitted: u64,
    pub command_queue_full: u64,
    pub command_queue_disconnected: u64,
    pub commands_processed: u64,
    pub worker_iterations: u64,
    pub input_ticks_submitted: u64,
    pub input_backpressure_retries: u64,
    pub snapshots_published: u64,
    pub snapshots_coalesced: u64,
    pub dropped_presentation_event_ticks: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RemoteOnlineClientStatus {
    pub generation: u64,
    pub phase: RemoteOnlineClientPhase,
    pub network_tick: SimTick,
    pub predicted_tick: Option<SimTick>,
    pub confirmed_tick: Option<SimTick>,
    /// Actual authority-selected countdown boundary retained for reconnect.
    pub countdown_start_tick: Option<SimTick>,
    pub quality: NetworkQualitySnapshot,
    pub protocol: ClientProtocolMetrics,
    pub rollback: RollbackMetrics,
    pub last_hard_resync_reason: Option<HardResyncReason>,
    pub runtime: RuntimeMetrics,
    pub worker: RemoteOnlineWorkerMetrics,
    pub failure: Option<OnlineFailure>,
    /// Authenticated authority-authored termination for this exact worker
    /// generation. It is cleared by constructing a reconnect generation.
    pub authority_disconnect: Option<RemoteAuthorityDisconnect>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemoteAuthorityDisconnect {
    pub generation: u64,
    pub message: DisconnectMessage,
    /// Local prediction progress is diagnostic context only. The authority's
    /// payload tick is likewise telemetry and neither value is a reconnect
    /// seed; reconnect always asks the authority for a retained snapshot.
    pub local_confirmed_tick: Option<SimTick>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteOnlineTerminal {
    Completed(ConfirmedSessionResult),
    Stopped,
    Failed(OnlineFailure),
    AuthorityDisconnected(RemoteAuthorityDisconnect),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RemoteLocalInputSample {
    pub seat: SeatId,
    pub movement_x: QuantizedAxis,
    pub movement_y: QuantizedAxis,
    pub held_buttons: InputButtons,
    pub pressed_buttons: InputButtons,
    pub released_buttons: InputButtons,
}

impl RemoteLocalInputSample {
    pub fn from_action_frame(frame: InputFrame) -> Self {
        Self {
            seat: frame.seat,
            movement_x: frame.movement_x,
            movement_y: frame.movement_y,
            held_buttons: frame.held_buttons,
            pressed_buttons: frame.pressed_buttons,
            released_buttons: frame.released_buttons,
        }
    }

    pub fn validate(self) -> Result<(), crate::network_protocol::ProtocolValidationError> {
        InputFrame {
            seat: self.seat,
            movement_x: self.movement_x,
            movement_y: self.movement_y,
            held_buttons: self.held_buttons,
            pressed_buttons: self.pressed_buttons,
            released_buttons: self.released_buttons,
            ..InputFrame::default()
        }
        .validate()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemoteLocalInputBatch {
    samples: [Option<RemoteLocalInputSample>; MAX_SEATS],
    len: u8,
}

impl RemoteLocalInputBatch {
    pub fn new(
        samples: &[RemoteLocalInputSample],
    ) -> Result<Self, crate::network_protocol::ProtocolValidationError> {
        use crate::network_protocol::ProtocolValidationError;

        if samples.is_empty() || samples.len() > MAX_SEATS {
            return Err(ProtocolValidationError::InvalidLocalSeatCount);
        }
        let mut batch = Self {
            samples: [None; MAX_SEATS],
            len: samples.len() as u8,
        };
        for sample in samples.iter().copied() {
            sample.validate()?;
            let slot = &mut batch.samples[usize::from(sample.seat.get())];
            if slot.is_some() {
                return Err(ProtocolValidationError::DuplicateInputSeat);
            }
            *slot = Some(sample);
        }
        Ok(batch)
    }

    pub const fn len(self) -> usize {
        self.len as usize
    }

    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    pub fn iter(self) -> impl Iterator<Item = RemoteLocalInputSample> {
        self.samples.into_iter().flatten()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteCommandSubmitOutcome {
    Queued,
    Full,
    Disconnected,
}

#[derive(Clone, Copy, Debug, Default)]
struct WorkerSeatInput {
    latest: Option<RemoteLocalInputSample>,
    next_sequence: InputSequence,
}

impl WorkerSeatInput {
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

#[derive(Clone, Debug, Default)]
struct WorkerRollbackHooks {
    pending_retain_through: Rc<Cell<Option<SimTick>>>,
}

impl WorkerRollbackHooks {
    fn take(&self) -> Option<SimTick> {
        self.pending_retain_through.take()
    }
}

impl RollbackEventDiscard for WorkerRollbackHooks {
    fn discard_after(&mut self, retained_through: SimTick) {
        let retained_through = self
            .pending_retain_through
            .get()
            .map_or(retained_through, |pending| pending.min(retained_through));
        self.pending_retain_through.set(Some(retained_through));
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct RemotePresentationEvent {
    event: SimEvent,
    combat: Option<CombatPresentationIntent>,
    combat_cue: Option<CombatPresentationCueIntent>,
    fighter: Option<FighterPresentationIntent>,
    item: Option<ItemPresentationIntent>,
    arena: Option<ArenaPresentationIntent>,
    special: Option<SpecialPresentationIntent>,
    bee: Option<BeePresentationIntent>,
    chick: Option<ChickPresentationIntent>,
    penguin: Option<PenguinPresentationIntent>,
}

#[derive(Debug)]
struct RemotePresentationTick {
    tick: SimTick,
    events: Vec<RemotePresentationEvent>,
}

impl RemotePresentationTick {
    fn capture(source: &World, tick: SimTick) -> Result<Option<Self>, OnlineFailure> {
        let journal = source
            .get_resource::<SimEventJournal>()
            .ok_or_else(internal_failure)?;
        let Some(events) = journal.events_at(tick) else {
            return Ok(None);
        };
        if events.len() > MAX_SIM_EVENTS_PER_TICK {
            return Err(internal_failure());
        }
        let combat = source.get_resource::<CombatPresentationIntentJournal>();
        let fighter = source.get_resource::<FighterPresentationIntentJournal>();
        let item = source.get_resource::<ItemPresentationIntentJournal>();
        let arena = source.get_resource::<ArenaPresentationIntentJournal>();
        let special = source.get_resource::<SpecialPresentationIntentJournal>();
        let bee = source.get_resource::<BeePresentationIntentJournal>();
        let chick = source.get_resource::<ChickPresentationIntentJournal>();
        let penguin = source.get_resource::<PenguinPresentationIntentJournal>();
        let events = events
            .iter()
            .flatten()
            .copied()
            .map(|event| RemotePresentationEvent {
                event,
                combat: combat.as_ref().and_then(|journal| journal.get(event.id)),
                combat_cue: combat.as_ref().and_then(|journal| journal.cue(event.id)),
                fighter: fighter.as_ref().and_then(|journal| journal.get(event.id)),
                item: item.as_ref().and_then(|journal| journal.get(event.id)),
                arena: arena.as_ref().and_then(|journal| journal.get(event.id)),
                special: special.as_ref().and_then(|journal| journal.get(event.id)),
                bee: bee.as_ref().and_then(|journal| journal.get(event.id)),
                chick: chick.as_ref().and_then(|journal| journal.get(event.id)),
                penguin: penguin.as_ref().and_then(|journal| journal.get(event.id)),
            })
            .collect();
        Ok(Some(Self { tick, events }))
    }
}

#[derive(Debug)]
struct RemoteProjectionFrame {
    snapshot: CanonicalSnapshot,
    confirmed_through: Option<SimTick>,
    /// Present only on the exact authority-confirmed final snapshot.
    confirmed_result: Option<ConfirmedSessionResult>,
}

struct RemoteMailboxState {
    frame: Option<RemoteProjectionFrame>,
    event_ticks: VecDeque<RemotePresentationTick>,
    rollback_retain_through: Option<SimTick>,
    status: RemoteOnlineClientStatus,
    status_version: u64,
    confirmed_result: Option<ConfirmedSessionResult>,
    terminal: Option<RemoteOnlineTerminal>,
}

impl RemoteMailboxState {
    fn new(generation: u64, reconnecting: bool) -> Self {
        Self {
            frame: None,
            event_ticks: VecDeque::new(),
            rollback_retain_through: None,
            status: RemoteOnlineClientStatus {
                generation,
                phase: if reconnecting {
                    RemoteOnlineClientPhase::Reconnecting
                } else {
                    RemoteOnlineClientPhase::Connecting
                },
                ..RemoteOnlineClientStatus::default()
            },
            status_version: 1,
            confirmed_result: None,
            terminal: None,
        }
    }

    fn retain_events_through(&mut self, retained_through: SimTick) {
        self.event_ticks
            .retain(|events| events.tick <= retained_through);
        self.rollback_retain_through = Some(
            self.rollback_retain_through
                .map_or(retained_through, |pending| pending.min(retained_through)),
        );
    }
}

struct RemoteMailboxPublisher {
    state: Arc<Mutex<RemoteMailboxState>>,
    signal: SyncSender<()>,
    metrics: Arc<SharedWorkerMetrics>,
}

impl RemoteMailboxPublisher {
    fn publish_status(&self, status: RemoteOnlineClientStatus) {
        {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            state.status = status;
            state.status_version = state.status_version.wrapping_add(1).max(1);
        }
        let _ = self.signal.try_send(());
    }

    fn publish_frame(
        &self,
        frame: RemoteProjectionFrame,
        rollback_retain_through: Option<SimTick>,
        event_ticks: Vec<RemotePresentationTick>,
        completed_status: Option<RemoteOnlineClientStatus>,
    ) {
        assert_eq!(
            frame.confirmed_result.is_some(),
            completed_status.is_some(),
            "only the authoritative final projection may publish Completed"
        );
        {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            if let Some(result) = frame.confirmed_result {
                if let Some(existing) = state.confirmed_result {
                    assert_eq!(
                        existing, result,
                        "one worker generation published conflicting confirmed results"
                    );
                } else {
                    state.confirmed_result = Some(result);
                }
            }
            if state.frame.is_some() {
                self.metrics
                    .snapshots_coalesced
                    .fetch_add(1, Ordering::Relaxed);
            }
            if let Some(retained_through) = rollback_retain_through {
                state.retain_events_through(retained_through);
            }
            for events in event_ticks {
                if let Some(existing) = state
                    .event_ticks
                    .iter_mut()
                    .find(|queued| queued.tick == events.tick)
                {
                    *existing = events;
                } else {
                    state.event_ticks.push_back(events);
                }
                while state.event_ticks.len() > SIM_EVENT_HISTORY_TICKS {
                    state.event_ticks.pop_front();
                    self.metrics
                        .dropped_presentation_event_ticks
                        .fetch_add(1, Ordering::Relaxed);
                }
            }
            state.frame = Some(frame);
            if let Some(mut status) = completed_status {
                let result = state
                    .confirmed_result
                    .expect("completed projection retained its confirmed result");
                status.phase = RemoteOnlineClientPhase::Results;
                status.failure = None;
                status.authority_disconnect = None;
                state.status = status;
                state.status_version = state.status_version.wrapping_add(1).max(1);
                state.terminal = Some(RemoteOnlineTerminal::Completed(result));
            }
        }
        self.metrics
            .snapshots_published
            .fetch_add(1, Ordering::Relaxed);
        let _ = self.signal.try_send(());
    }

    fn finish(&self, terminal: RemoteOnlineTerminal, mut status: RemoteOnlineClientStatus) {
        status.phase = match terminal {
            RemoteOnlineTerminal::Completed(_) => RemoteOnlineClientPhase::Results,
            RemoteOnlineTerminal::Stopped => RemoteOnlineClientPhase::Stopped,
            RemoteOnlineTerminal::Failed(failure) => {
                status.failure = Some(failure);
                RemoteOnlineClientPhase::Failed
            }
            RemoteOnlineTerminal::AuthorityDisconnected(disconnect) => {
                let failure = OnlineFailure::from_disconnect(disconnect.message);
                status.failure = Some(failure);
                status.authority_disconnect = Some(disconnect);
                match failure.recovery {
                    OnlineRecoveryAction::Reconnect => RemoteOnlineClientPhase::Reconnecting,
                    OnlineRecoveryAction::MatchEndedNoContest => RemoteOnlineClientPhase::Results,
                    OnlineRecoveryAction::Dismiss
                    | OnlineRecoveryAction::Retry
                    | OnlineRecoveryAction::ReturnToLobby
                    | OnlineRecoveryAction::ReturnToMenu
                    | OnlineRecoveryAction::DisableOnline => RemoteOnlineClientPhase::Failed,
                }
            }
        };
        {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            if let RemoteOnlineTerminal::Completed(result) = terminal {
                assert_eq!(
                    state.confirmed_result,
                    Some(result),
                    "completed terminal must follow its atomic final projection frame"
                );
            }
            state.status = status;
            state.status_version = state.status_version.wrapping_add(1).max(1);
            state.terminal = Some(terminal);
        }
        let _ = self.signal.try_send(());
    }
}

struct RemoteMailboxDrain {
    frame: Option<RemoteProjectionFrame>,
    event_ticks: Vec<RemotePresentationTick>,
    rollback_retain_through: Option<SimTick>,
    status: RemoteOnlineClientStatus,
    confirmed_result: Option<ConfirmedSessionResult>,
    terminal: Option<RemoteOnlineTerminal>,
}

struct RemoteMailboxInbox {
    state: Arc<Mutex<RemoteMailboxState>>,
    signal: Receiver<()>,
}

impl RemoteMailboxInbox {
    fn drain(&mut self) -> RemoteMailboxDrain {
        while self.signal.try_recv().is_ok() {}
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        RemoteMailboxDrain {
            frame: state.frame.take(),
            event_ticks: state.event_ticks.drain(..).collect(),
            rollback_retain_through: state.rollback_retain_through.take(),
            status: state.status,
            confirmed_result: state.confirmed_result,
            terminal: state.terminal,
        }
    }

    fn latest_status(&self) -> RemoteOnlineClientStatus {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .status
    }

    fn confirmed_result(&self) -> Option<ConfirmedSessionResult> {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .confirmed_result
    }

    fn terminal(&self) -> Option<RemoteOnlineTerminal> {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .terminal
    }

    #[cfg(test)]
    fn wait_for_iteration(&self, expected: u64, timeout: Duration) -> bool {
        let deadline = Instant::now().checked_add(timeout);
        loop {
            if self.latest_status().worker.worker_iterations >= expected {
                return true;
            }
            if self
                .terminal()
                .is_some_and(|terminal| !matches!(terminal, RemoteOnlineTerminal::Completed(_)))
            {
                return false;
            }
            let remaining = deadline
                .map(|deadline| deadline.saturating_duration_since(Instant::now()))
                .unwrap_or(timeout);
            match self.signal.recv_timeout(remaining) {
                Ok(()) => {}
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => return false,
            }
        }
    }
}

#[derive(Default)]
struct SharedWorkerMetrics {
    command_queue_depth: AtomicUsize,
    command_queue_high_water: AtomicUsize,
    input_commands_submitted: AtomicU64,
    quality_commands_submitted: AtomicU64,
    command_queue_full: AtomicU64,
    command_queue_disconnected: AtomicU64,
    commands_processed: AtomicU64,
    worker_iterations: AtomicU64,
    input_ticks_submitted: AtomicU64,
    input_backpressure_retries: AtomicU64,
    snapshots_published: AtomicU64,
    snapshots_coalesced: AtomicU64,
    dropped_presentation_event_ticks: AtomicU64,
}

impl SharedWorkerMetrics {
    fn snapshot(&self, capacity: usize) -> RemoteOnlineWorkerMetrics {
        RemoteOnlineWorkerMetrics {
            command_queue_depth: self
                .command_queue_depth
                .load(Ordering::Relaxed)
                .min(capacity),
            command_queue_high_water: self
                .command_queue_high_water
                .load(Ordering::Relaxed)
                .min(capacity),
            input_commands_submitted: self.input_commands_submitted.load(Ordering::Relaxed),
            quality_commands_submitted: self.quality_commands_submitted.load(Ordering::Relaxed),
            command_queue_full: self.command_queue_full.load(Ordering::Relaxed),
            command_queue_disconnected: self.command_queue_disconnected.load(Ordering::Relaxed),
            commands_processed: self.commands_processed.load(Ordering::Relaxed),
            worker_iterations: self.worker_iterations.load(Ordering::Relaxed),
            input_ticks_submitted: self.input_ticks_submitted.load(Ordering::Relaxed),
            input_backpressure_retries: self.input_backpressure_retries.load(Ordering::Relaxed),
            snapshots_published: self.snapshots_published.load(Ordering::Relaxed),
            snapshots_coalesced: self.snapshots_coalesced.load(Ordering::Relaxed),
            dropped_presentation_event_ticks: self
                .dropped_presentation_event_ticks
                .load(Ordering::Relaxed),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum RemoteWorkerCommand {
    Input(RemoteLocalInputBatch),
    Quality(NetworkQualitySample),
    #[cfg(test)]
    AdvanceManual(u16),
    Stop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RemoteWorkerClockMode {
    Realtime,
    #[cfg(test)]
    Manual,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RemoteBootstrapMode {
    Initial,
    Reconnect { countdown_start_tick: SimTick },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RemoteWorkerFailure {
    Generic(OnlineFailure),
    AuthorityDisconnect(RemoteAuthorityDisconnect),
}

impl From<OnlineFailure> for RemoteWorkerFailure {
    fn from(failure: OnlineFailure) -> Self {
        Self::Generic(failure)
    }
}

#[derive(Clone)]
struct RemoteClientBootstrap {
    match_config: HeadlessMatchConfig,
    peer_id: PeerId,
    client_config: RemoteOnlineClientConfig,
}

/// Main-thread owner for one remote predicted client worker.
pub struct RemoteOnlineClient {
    bootstrap: RemoteClientBootstrap,
    generation: u64,
    commands: Option<SyncSender<RemoteWorkerCommand>>,
    force_shutdown: Arc<AtomicBool>,
    content_ready: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    inbox: RemoteMailboxInbox,
    shared_metrics: Arc<SharedWorkerMetrics>,
    projector: LivePresentationProjector,
    projection_source: World,
    presentation_prepared: bool,
    sample_tick: u64,
    pending_local_samples: [Option<RemoteLocalInputSample>; MAX_SEATS],
}

impl RemoteOnlineClient {
    pub fn spawn<E>(
        endpoint: E,
        match_config: HeadlessMatchConfig,
        peer_id: PeerId,
        client_config: RemoteOnlineClientConfig,
    ) -> Result<Self, RemoteOnlineClientStartError>
    where
        E: NonBlockingDatagramEndpoint + Send + 'static,
    {
        let bootstrap = RemoteClientBootstrap {
            match_config,
            peer_id,
            client_config,
        };
        validate_bootstrap(&bootstrap)?;
        Self::spawn_inner(
            endpoint,
            bootstrap,
            1,
            RemoteBootstrapMode::Initial,
            RemoteWorkerClockMode::Realtime,
        )
    }

    fn spawn_inner<E>(
        endpoint: E,
        bootstrap: RemoteClientBootstrap,
        generation: u64,
        mode: RemoteBootstrapMode,
        clock: RemoteWorkerClockMode,
    ) -> Result<Self, RemoteOnlineClientStartError>
    where
        E: NonBlockingDatagramEndpoint + Send + 'static,
    {
        let (command_tx, command_rx) = mpsc::sync_channel(bootstrap.client_config.command_capacity);
        let force_shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = Arc::clone(&force_shutdown);
        let content_ready = Arc::new(AtomicBool::new(matches!(
            mode,
            RemoteBootstrapMode::Reconnect { .. }
        )));
        let worker_content_ready = Arc::clone(&content_ready);
        let shared_metrics = Arc::new(SharedWorkerMetrics::default());
        let worker_metrics = Arc::clone(&shared_metrics);
        let mailbox = Arc::new(Mutex::new(RemoteMailboxState::new(
            generation,
            matches!(mode, RemoteBootstrapMode::Reconnect { .. }),
        )));
        let (signal_tx, signal_rx) = mpsc::sync_channel(REMOTE_CLIENT_SIGNAL_CAPACITY);
        let publisher = RemoteMailboxPublisher {
            state: Arc::clone(&mailbox),
            signal: signal_tx,
            metrics: Arc::clone(&shared_metrics),
        };
        let worker_bootstrap = bootstrap.clone();
        let join = thread::Builder::new()
            .name("afc-remote-client-60hz".to_owned())
            .spawn(move || {
                run_remote_client_worker(
                    endpoint,
                    worker_bootstrap,
                    generation,
                    mode,
                    clock,
                    command_rx,
                    &worker_shutdown,
                    &worker_content_ready,
                    worker_metrics,
                    publisher,
                );
            })
            .map_err(RemoteOnlineClientStartError::Spawn)?;

        Ok(Self {
            bootstrap,
            generation,
            commands: Some(command_tx),
            force_shutdown,
            content_ready,
            join: Some(join),
            inbox: RemoteMailboxInbox {
                state: mailbox,
                signal: signal_rx,
            },
            shared_metrics,
            projector: LivePresentationProjector::new(),
            projection_source: projection_source_world(),
            presentation_prepared: false,
            sample_tick: 0,
            pending_local_samples: [None; MAX_SEATS],
        })
    }

    pub const fn manifest(&self) -> &MatchManifest {
        &self.bootstrap.match_config.manifest
    }

    pub const fn peer_id(&self) -> PeerId {
        self.bootstrap.peer_id
    }

    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub fn status(&self) -> RemoteOnlineClientStatus {
        self.inbox.latest_status()
    }

    pub fn confirmed_result(&self) -> Option<ConfirmedSessionResult> {
        self.inbox.confirmed_result()
    }

    /// Releases the manifest/loading gate after all simulation and presentation
    /// definitions referenced by the agreed manifest are resident.
    pub fn mark_content_loaded(&self) {
        self.content_ready.store(true, Ordering::Release);
    }

    pub fn terminal(&self) -> Option<RemoteOnlineTerminal> {
        self.inbox.terminal()
    }

    pub fn metrics(&self) -> RemoteOnlineWorkerMetrics {
        self.shared_metrics
            .snapshot(self.bootstrap.client_config.command_capacity)
    }

    pub fn submit_inputs(&self, batch: RemoteLocalInputBatch) -> RemoteCommandSubmitOutcome {
        self.try_submit(RemoteWorkerCommand::Input(batch), true)
    }

    pub fn submit_quality_sample(
        &self,
        sample: NetworkQualitySample,
    ) -> Result<RemoteCommandSubmitOutcome, NetworkQualityError> {
        sample.validate()?;
        Ok(self.try_submit(RemoteWorkerCommand::Quality(sample), false))
    }

    /// Drains one action sample for every local seat owned by this Steam peer.
    /// The worker reticks and resequences the samples against the synchronized
    /// authority clock; this local ordinal exists only for gesture recognition.
    pub fn sample_local_inputs(
        &mut self,
        local_inputs: &mut LocalTickInputState,
    ) -> Result<RemoteCommandSubmitOutcome, crate::network_protocol::ProtocolValidationError> {
        self.sample_tick = self.sample_tick.saturating_add(1);
        let manifest = *self.manifest();
        let peer_id = self.peer_id();
        let mut local_index = 0;
        for assignment in manifest.ownership.as_slice() {
            if assignment.owner != SeatOwner::Peer(peer_id) {
                continue;
            }
            // Protocol seats are global canonical fighter slots. Couch input
            // sources are local ordinals within this peer and need not have the
            // same numbers (for example, remote peer two may own global seat 3
            // using its local controller zero).
            let local_seat = LocalSeatId::new(local_index)
                .ok_or(crate::network_protocol::ProtocolValidationError::InvalidSeat)?;
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
                return Err(
                    crate::network_protocol::ProtocolValidationError::InvalidLocalSeatCount,
                );
            };
            samples[count] = sample;
            count += 1;
        }
        let outcome = self.submit_inputs(RemoteLocalInputBatch::new(&samples[..count])?);
        if outcome == RemoteCommandSubmitOutcome::Queued {
            for assignment in manifest.ownership.as_slice() {
                if assignment.owner == SeatOwner::Peer(peer_id) {
                    self.pending_local_samples[usize::from(assignment.seat.get())] = None;
                }
            }
        }
        Ok(outcome)
    }

    pub fn prepare_presentation_target(
        &mut self,
        target: &mut World,
    ) -> Result<(), RemoteOnlinePresentationError> {
        if !target.contains_resource::<ConfirmedProgressionLedger>() {
            target.insert_resource(ConfirmedProgressionLedger::default());
        }
        let manifest = *self.manifest();
        self.projector
            .prepare_target(target, &manifest)
            .map_err(RemoteOnlinePresentationError::Projection)?;
        self.presentation_prepared = true;
        Ok(())
    }

    /// Applies the latest predicted canonical snapshot and all rollback-safe
    /// sidecars currently waiting for the render thread. Intermediate snapshots
    /// may be coalesced, while confirmed results remain retained.
    pub fn project_latest(
        &mut self,
        target: &mut World,
    ) -> Result<RemoteOnlineClientUpdate, RemoteOnlinePresentationError> {
        if !self.presentation_prepared {
            self.prepare_presentation_target(target)?;
        }
        let drain = self.inbox.drain();
        if let Some(retained_through) = drain.rollback_retain_through {
            discard_projection_after(
                &mut self.projection_source,
                &mut self.projector,
                retained_through,
            );
        }
        for events in drain.event_ticks {
            install_presentation_tick(&mut self.projection_source, events)?;
        }

        let mut projected_confirmed_result = None;
        let projection = if let Some(frame) = drain.frame {
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
                    self.peer_id(),
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
            status: drain.status,
            confirmed_result: drain.confirmed_result,
            projected_confirmed_result,
            terminal: drain.terminal,
        })
    }

    /// Replaces a failed connection with a reconnect endpoint while preserving
    /// immutable match/peer identity. The old worker is fully joined first, and
    /// stale publications cannot cross the incremented generation boundary.
    pub fn reconnect<E>(&mut self, endpoint: E) -> Result<(), RemoteOnlineClientStartError>
    where
        E: NonBlockingDatagramEndpoint + Send + 'static,
    {
        self.reconnect_with_clock(endpoint, RemoteWorkerClockMode::Realtime)
    }

    fn reconnect_with_clock<E>(
        &mut self,
        endpoint: E,
        clock: RemoteWorkerClockMode,
    ) -> Result<(), RemoteOnlineClientStartError>
    where
        E: NonBlockingDatagramEndpoint + Send + 'static,
    {
        self.stop_worker();
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(RemoteOnlineClientStartError::GenerationExhausted)?;
        let countdown_start_tick = self
            .status()
            .countdown_start_tick
            .ok_or(RemoteOnlineClientStartError::ReconnectBoundaryUnavailable)?;
        let replacement = Self::spawn_inner(
            endpoint,
            self.bootstrap.clone(),
            generation,
            RemoteBootstrapMode::Reconnect {
                countdown_start_tick,
            },
            clock,
        )?;
        *self = replacement;
        Ok(())
    }

    pub fn stop(&mut self) {
        self.stop_worker();
    }

    fn stop_worker(&mut self) {
        self.force_shutdown.store(true, Ordering::Release);
        if let Some(commands) = self.commands.take() {
            let _ = commands.try_send(RemoteWorkerCommand::Stop);
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }

    fn try_submit(&self, command: RemoteWorkerCommand, input: bool) -> RemoteCommandSubmitOutcome {
        let Some(commands) = self.commands.as_ref() else {
            self.shared_metrics
                .command_queue_disconnected
                .fetch_add(1, Ordering::Relaxed);
            return RemoteCommandSubmitOutcome::Disconnected;
        };
        let depth = self
            .shared_metrics
            .command_queue_depth
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1);
        match commands.try_send(command) {
            Ok(()) => {
                let _ = self.shared_metrics.command_queue_high_water.fetch_max(
                    depth.min(self.bootstrap.client_config.command_capacity),
                    Ordering::Relaxed,
                );
                if input {
                    self.shared_metrics
                        .input_commands_submitted
                        .fetch_add(1, Ordering::Relaxed);
                } else {
                    self.shared_metrics
                        .quality_commands_submitted
                        .fetch_add(1, Ordering::Relaxed);
                }
                RemoteCommandSubmitOutcome::Queued
            }
            Err(TrySendError::Full(_)) => {
                self.shared_metrics
                    .command_queue_depth
                    .fetch_sub(1, Ordering::AcqRel);
                self.shared_metrics
                    .command_queue_full
                    .fetch_add(1, Ordering::Relaxed);
                RemoteCommandSubmitOutcome::Full
            }
            Err(TrySendError::Disconnected(_)) => {
                self.shared_metrics
                    .command_queue_depth
                    .fetch_sub(1, Ordering::AcqRel);
                self.shared_metrics
                    .command_queue_disconnected
                    .fetch_add(1, Ordering::Relaxed);
                RemoteCommandSubmitOutcome::Disconnected
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn spawn_manual<E>(
        endpoint: E,
        match_config: HeadlessMatchConfig,
        peer_id: PeerId,
        client_config: RemoteOnlineClientConfig,
    ) -> Result<Self, RemoteOnlineClientStartError>
    where
        E: NonBlockingDatagramEndpoint + Send + 'static,
    {
        let bootstrap = RemoteClientBootstrap {
            match_config,
            peer_id,
            client_config,
        };
        validate_bootstrap(&bootstrap)?;
        Self::spawn_inner(
            endpoint,
            bootstrap,
            1,
            RemoteBootstrapMode::Initial,
            RemoteWorkerClockMode::Manual,
        )
    }

    #[cfg(test)]
    pub(crate) fn advance_manual(&self, iterations: u16) -> bool {
        if iterations == 0 {
            return false;
        }
        let before = self.metrics().worker_iterations;
        if !matches!(
            self.try_submit(RemoteWorkerCommand::AdvanceManual(iterations), false),
            RemoteCommandSubmitOutcome::Queued
        ) {
            return false;
        }
        self.inbox.wait_for_iteration(
            before.saturating_add(u64::from(iterations)),
            Duration::from_secs(5),
        )
    }

    #[cfg(test)]
    pub(crate) fn reconnect_manual<E>(
        &mut self,
        endpoint: E,
    ) -> Result<(), RemoteOnlineClientStartError>
    where
        E: NonBlockingDatagramEndpoint + Send + 'static,
    {
        self.reconnect_with_clock(endpoint, RemoteWorkerClockMode::Manual)
    }
}

impl Drop for RemoteOnlineClient {
    fn drop(&mut self) {
        self.stop_worker();
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RemoteOnlineClientUpdate {
    pub projection: Option<LivePresentationProjectionReport>,
    pub status: RemoteOnlineClientStatus,
    pub confirmed_result: Option<ConfirmedSessionResult>,
    /// Result whose exact final frame was successfully projected and whose
    /// progression/presentation transaction was installed in this call.
    pub projected_confirmed_result: Option<ConfirmedSessionResult>,
    pub terminal: Option<RemoteOnlineTerminal>,
}

fn validate_bootstrap(
    bootstrap: &RemoteClientBootstrap,
) -> Result<(), RemoteOnlineClientStartError> {
    bootstrap
        .client_config
        .validate()
        .map_err(RemoteOnlineClientStartError::InvalidConfig)?;
    bootstrap
        .match_config
        .validate()
        .map_err(RemoteOnlineClientStartError::InvalidMatch)?;
    if u32::from(bootstrap.match_config.manifest.tick_rate_hz) != AUTHORITY_THREAD_TICK_RATE_HZ {
        return Err(RemoteOnlineClientStartError::TickRateMismatch {
            manifest_hz: bootstrap.match_config.manifest.tick_rate_hz,
        });
    }
    if !bootstrap
        .match_config
        .manifest
        .ownership
        .peer_owns_any_seat(bootstrap.peer_id)
    {
        return Err(RemoteOnlineClientStartError::PeerOwnsNoSeat);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_remote_client_worker<E>(
    endpoint: E,
    bootstrap: RemoteClientBootstrap,
    generation: u64,
    mode: RemoteBootstrapMode,
    clock: RemoteWorkerClockMode,
    commands: Receiver<RemoteWorkerCommand>,
    force_shutdown: &AtomicBool,
    content_ready: &AtomicBool,
    shared_metrics: Arc<SharedWorkerMetrics>,
    publisher: RemoteMailboxPublisher,
) where
    E: NonBlockingDatagramEndpoint,
{
    let mut status = RemoteOnlineClientStatus {
        generation,
        phase: if matches!(mode, RemoteBootstrapMode::Reconnect { .. }) {
            RemoteOnlineClientPhase::Reconnecting
        } else {
            RemoteOnlineClientPhase::Connecting
        },
        ..RemoteOnlineClientStatus::default()
    };
    let outcome = run_remote_client_worker_inner(
        endpoint,
        &bootstrap,
        mode,
        clock,
        commands,
        force_shutdown,
        content_ready,
        &shared_metrics,
        &publisher,
        &mut status,
    );
    match outcome {
        // Completed is installed atomically with the final projection frame.
        // An explicit Stop after Results only joins the endpoint owner; it must
        // not replace that authoritative terminal with Stopped.
        Ok(Some(_)) => {}
        Ok(None) => publisher.finish(RemoteOnlineTerminal::Stopped, status),
        Err(RemoteWorkerFailure::Generic(failure)) => {
            publisher.finish(RemoteOnlineTerminal::Failed(failure), status)
        }
        Err(RemoteWorkerFailure::AuthorityDisconnect(disconnect)) => publisher.finish(
            RemoteOnlineTerminal::AuthorityDisconnected(disconnect),
            status,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn run_remote_client_worker_inner<E>(
    endpoint: E,
    bootstrap: &RemoteClientBootstrap,
    mode: RemoteBootstrapMode,
    clock: RemoteWorkerClockMode,
    commands: Receiver<RemoteWorkerCommand>,
    force_shutdown: &AtomicBool,
    content_ready: &AtomicBool,
    shared_metrics: &SharedWorkerMetrics,
    publisher: &RemoteMailboxPublisher,
    status: &mut RemoteOnlineClientStatus,
) -> Result<Option<ConfirmedSessionResult>, RemoteWorkerFailure>
where
    E: NonBlockingDatagramEndpoint,
{
    let generation = status.generation;
    let predicted = build_predicted_simulation(bootstrap.match_config.clone())
        .map_err(|_| internal_failure())?;
    let rollback = WorkerRollbackHooks::default();
    let predicted = PredictedClient::with_hooks(
        predicted,
        bootstrap.match_config.manifest.match_id,
        usize::from(bootstrap.match_config.manifest.snapshot_history_ticks),
        rollback.clone(),
        InstantRollbackTiming::default(),
    )
    .map_err(|_| synchronization_failure())?;
    let initial_time = ClientProtocolTime::default();
    let mut protocol = match mode {
        RemoteBootstrapMode::Initial => RemotePredictedClientProtocol::new(
            endpoint,
            bootstrap.match_config.manifest.match_id,
            bootstrap.peer_id,
            bootstrap.match_config.manifest.compatibility,
            predicted,
            bootstrap.client_config.protocol,
            initial_time,
        ),
        // The reconnect constructor is supplied by `client_protocol`: it binds
        // the already-agreed manifest and expects the authority's unsolicited
        // `Reconnect` transfer after the fresh transport handshake.
        RemoteBootstrapMode::Reconnect {
            countdown_start_tick,
        } => RemotePredictedClientProtocol::new_reconnect(
            endpoint,
            bootstrap.match_config.manifest,
            bootstrap.peer_id,
            countdown_start_tick,
            predicted,
            bootstrap.client_config.protocol,
            initial_time,
        ),
    }
    .map_err(map_protocol_build_error)?;
    let mut quality = NetworkQualityMonitor::new(bootstrap.client_config.quality_policy)
        .map_err(|_| internal_failure())?;
    let mut inputs = [WorkerSeatInput::default(); MAX_SEATS];
    let mut pending_input_ticks = None;
    let mut content_loaded = matches!(mode, RemoteBootstrapMode::Reconnect { .. });
    let mut last_published_tick = None;
    let mut last_confirmed_result = None;
    let mut gameplay_link_retired = false;
    let mut last_clock_samples = 0;
    let mut schedule = SixtyHzSchedule::new();
    let epoch = Instant::now();
    let mut network_tick = SimTick::ZERO;
    #[cfg(test)]
    let mut manual_iterations = 0_u32;

    loop {
        if force_shutdown.load(Ordering::Acquire) {
            return Ok(last_confirmed_result);
        }
        let should_service = match clock {
            RemoteWorkerClockMode::Realtime => wait_for_realtime_iteration(
                &commands,
                force_shutdown,
                &mut inputs,
                &mut quality,
                &schedule,
                bootstrap.client_config.max_commands_per_service,
                shared_metrics,
                &bootstrap.match_config.manifest,
                bootstrap.peer_id,
            )?,
            #[cfg(test)]
            RemoteWorkerClockMode::Manual => wait_for_manual_iteration(
                &commands,
                force_shutdown,
                &mut inputs,
                &mut quality,
                &mut manual_iterations,
                bootstrap.client_config.max_commands_per_service,
                shared_metrics,
                &bootstrap.match_config.manifest,
                bootstrap.peer_id,
            )?,
        };
        if !should_service {
            return Ok(last_confirmed_result);
        }
        network_tick = network_tick
            .0
            .checked_add(1)
            .map(SimTick)
            .ok_or_else(clock_failure)?;
        let monotonic_micros = match clock {
            RemoteWorkerClockMode::Realtime => {
                epoch.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
            }
            #[cfg(test)]
            RemoteWorkerClockMode::Manual => {
                network_tick.get().saturating_mul(MICROS_PER_SECOND)
                    / u64::from(AUTHORITY_THREAD_TICK_RATE_HZ)
            }
        };
        let now = ClientProtocolTime {
            network_tick,
            monotonic_micros,
        };

        if gameplay_link_retired {
            shared_metrics
                .worker_iterations
                .fetch_add(1, Ordering::Relaxed);
            status.network_tick = network_tick;
            status.worker = shared_metrics.snapshot(bootstrap.client_config.command_capacity);
            publisher.publish_status(*status);
            if matches!(clock, RemoteWorkerClockMode::Realtime) {
                schedule.advance();
            }
            continue;
        }

        let local_confirmed_tick = protocol.predicted_client().confirmed_tick();
        if let Err(error) = protocol.pump(now) {
            if last_confirmed_result.is_some() && is_post_result_transport_retirement(&error) {
                gameplay_link_retired = true;
                shared_metrics
                    .worker_iterations
                    .fetch_add(1, Ordering::Relaxed);
                status.network_tick = network_tick;
                status.worker = shared_metrics.snapshot(bootstrap.client_config.command_capacity);
                publisher.publish_status(*status);
                if matches!(clock, RemoteWorkerClockMode::Realtime) {
                    schedule.advance();
                }
                continue;
            }
            return Err(map_protocol_error(error, generation, local_confirmed_tick));
        }
        if let Some(received) = protocol.manifest().copied() {
            if received != bootstrap.match_config.manifest {
                return Err(incompatible_failure().into());
            }
            if !content_loaded && content_ready.load(Ordering::Acquire) {
                protocol.mark_content_loaded(now).map_err(|error| {
                    map_protocol_error(
                        error,
                        generation,
                        protocol.predicted_client().confirmed_tick(),
                    )
                })?;
                content_loaded = true;
            }
        }

        let submitted = if last_confirmed_result.is_none() {
            service_due_inputs(
                &mut protocol,
                &mut inputs,
                &mut pending_input_ticks,
                shared_metrics,
                generation,
            )?
        } else {
            false
        };
        if submitted {
            let local_confirmed_tick = protocol.predicted_client().confirmed_tick();
            protocol
                .pump(now)
                .map_err(|error| map_protocol_error(error, generation, local_confirmed_tick))?;
        }

        let clock_metrics = protocol.session_clock_metrics();
        if clock_metrics.accepted_samples != last_clock_samples {
            last_clock_samples = clock_metrics.accepted_samples;
            let rtt_ms = clock_metrics
                .best_rtt_micros
                .saturating_add(999)
                .saturating_div(1_000)
                .min(u32::from(u16::MAX)) as u16;
            quality
                .observe(NetworkQualitySample {
                    rtt_ms,
                    loss_bps: 0,
                })
                .map_err(|_| internal_failure())?;
        }

        let confirmed_result = protocol.take_confirmed_result();
        shared_metrics
            .worker_iterations
            .fetch_add(1, Ordering::Relaxed);
        *status = make_status(
            generation_from_status(status),
            mode,
            &protocol,
            network_tick,
            quality.snapshot(),
            shared_metrics.snapshot(bootstrap.client_config.command_capacity),
        );
        publish_protocol_projection(
            &protocol,
            &rollback,
            publisher,
            &mut last_published_tick,
            confirmed_result,
            confirmed_result.map(|_| *status),
        )?;
        if let Some(result) = confirmed_result {
            last_confirmed_result = Some(result);
        } else {
            publisher.publish_status(*status);
        }
        if matches!(clock, RemoteWorkerClockMode::Realtime) {
            schedule.advance();
        }
    }
}

fn generation_from_status(status: &RemoteOnlineClientStatus) -> u64 {
    status.generation
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

fn make_status<E>(
    generation: u64,
    mode: RemoteBootstrapMode,
    protocol: &LiveRemoteProtocol<E>,
    network_tick: SimTick,
    quality: NetworkQualitySnapshot,
    worker: RemoteOnlineWorkerMetrics,
) -> RemoteOnlineClientStatus
where
    E: NonBlockingDatagramEndpoint,
{
    let phase = if matches!(mode, RemoteBootstrapMode::Reconnect { .. })
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

fn service_due_inputs<E>(
    protocol: &mut LiveRemoteProtocol<E>,
    inputs: &mut [WorkerSeatInput; MAX_SEATS],
    pending: &mut Option<(SimTick, SimTick)>,
    metrics: &SharedWorkerMetrics,
    generation: u64,
) -> Result<bool, RemoteWorkerFailure>
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
        // Build against a copy so transient runtime backpressure cannot consume
        // an edge or advance a sequence before the protocol accepts the tick.
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
                metrics
                    .input_ticks_submitted
                    .fetch_add(1, Ordering::Relaxed);
                *pending = if tick < last {
                    Some((tick.next(), last))
                } else {
                    None
                };
            }
            Err(ClientProtocolError::Recoverable(
                ClientProtocolRecoverableError::OutboundBackpressure,
            )) => {
                metrics
                    .input_backpressure_retries
                    .fetch_add(1, Ordering::Relaxed);
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

#[allow(clippy::too_many_arguments)]
fn wait_for_realtime_iteration(
    commands: &Receiver<RemoteWorkerCommand>,
    force_shutdown: &AtomicBool,
    inputs: &mut [WorkerSeatInput; MAX_SEATS],
    quality: &mut NetworkQualityMonitor,
    schedule: &SixtyHzSchedule,
    max_commands: usize,
    metrics: &SharedWorkerMetrics,
    manifest: &MatchManifest,
    peer_id: PeerId,
) -> Result<bool, OnlineFailure> {
    loop {
        if force_shutdown.load(Ordering::Acquire) {
            return Ok(false);
        }
        let now = Instant::now();
        let deadline = schedule.deadline();
        if now >= deadline {
            break;
        }
        match commands.recv_timeout(deadline.duration_since(now)) {
            Ok(command) => {
                dequeue_command(metrics);
                if !service_worker_command(command, inputs, quality, manifest, peer_id)? {
                    return Ok(false);
                }
            }
            Err(RecvTimeoutError::Timeout) => break,
            Err(RecvTimeoutError::Disconnected) => return Ok(false),
        }
    }
    for _ in 0..max_commands {
        match commands.try_recv() {
            Ok(command) => {
                dequeue_command(metrics);
                if !service_worker_command(command, inputs, quality, manifest, peer_id)? {
                    return Ok(false);
                }
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => return Ok(false),
        }
    }
    Ok(!force_shutdown.load(Ordering::Acquire))
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn wait_for_manual_iteration(
    commands: &Receiver<RemoteWorkerCommand>,
    force_shutdown: &AtomicBool,
    inputs: &mut [WorkerSeatInput; MAX_SEATS],
    quality: &mut NetworkQualityMonitor,
    manual_iterations: &mut u32,
    max_commands: usize,
    metrics: &SharedWorkerMetrics,
    manifest: &MatchManifest,
    peer_id: PeerId,
) -> Result<bool, OnlineFailure> {
    while *manual_iterations == 0 {
        if force_shutdown.load(Ordering::Acquire) {
            return Ok(false);
        }
        let command = match commands.recv() {
            Ok(command) => command,
            Err(_) => return Ok(false),
        };
        dequeue_command(metrics);
        match command {
            RemoteWorkerCommand::AdvanceManual(iterations) => {
                *manual_iterations = manual_iterations.saturating_add(u32::from(iterations));
            }
            command => {
                if !service_worker_command(command, inputs, quality, manifest, peer_id)? {
                    return Ok(false);
                }
            }
        }
    }
    for _ in 0..max_commands.saturating_sub(1) {
        match commands.try_recv() {
            Ok(RemoteWorkerCommand::AdvanceManual(iterations)) => {
                dequeue_command(metrics);
                *manual_iterations = manual_iterations.saturating_add(u32::from(iterations));
            }
            Ok(command) => {
                dequeue_command(metrics);
                if !service_worker_command(command, inputs, quality, manifest, peer_id)? {
                    return Ok(false);
                }
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => return Ok(false),
        }
    }
    *manual_iterations -= 1;
    Ok(true)
}

fn dequeue_command(metrics: &SharedWorkerMetrics) {
    let _ =
        metrics
            .command_queue_depth
            .fetch_update(Ordering::AcqRel, Ordering::Relaxed, |depth| {
                Some(depth.saturating_sub(1))
            });
    metrics.commands_processed.fetch_add(1, Ordering::Relaxed);
}

fn service_worker_command(
    command: RemoteWorkerCommand,
    inputs: &mut [WorkerSeatInput; MAX_SEATS],
    quality: &mut NetworkQualityMonitor,
    manifest: &MatchManifest,
    peer_id: PeerId,
) -> Result<bool, OnlineFailure> {
    match command {
        RemoteWorkerCommand::Input(batch) => {
            let expected = manifest
                .ownership
                .as_slice()
                .iter()
                .filter(|assignment| assignment.owner == SeatOwner::Peer(peer_id))
                .count();
            if batch.len() != expected {
                return Err(invalid_input_failure());
            }
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
                inputs[usize::from(sample.seat.get())].merge(sample);
            }
            Ok(true)
        }
        RemoteWorkerCommand::Quality(sample) => {
            quality.observe(sample).map_err(|_| internal_failure())?;
            Ok(true)
        }
        #[cfg(test)]
        RemoteWorkerCommand::AdvanceManual(_) => Err(internal_failure()),
        RemoteWorkerCommand::Stop => Ok(false),
    }
}

fn publish_protocol_projection<E>(
    protocol: &LiveRemoteProtocol<E>,
    rollback: &WorkerRollbackHooks,
    publisher: &RemoteMailboxPublisher,
    last_published_tick: &mut Option<SimTick>,
    confirmed_result: Option<ConfirmedSessionResult>,
    completed_status: Option<RemoteOnlineClientStatus>,
) -> Result<(), OnlineFailure>
where
    E: NonBlockingDatagramEndpoint,
{
    let client = protocol.predicted_client();
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
    let mut rollback_retain_through = rollback.take();
    if confirmed_result.is_some() && target_tick < predicted_tick {
        rollback_retain_through =
            Some(rollback_retain_through.map_or(target_tick, |pending| pending.min(target_tick)));
    }
    if confirmed_result.is_none()
        && rollback_retain_through.is_none()
        && *last_published_tick == Some(target_tick)
    {
        return Ok(());
    }
    let driver = client.world();
    let source = driver.world();
    let journal = source
        .get_resource::<SimEventJournal>()
        .ok_or_else(internal_failure)?;
    let start = rollback_retain_through
        .and_then(|tick| tick.0.checked_add(1).map(SimTick))
        .or_else(|| last_published_tick.and_then(|tick| tick.0.checked_add(1).map(SimTick)))
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
        driver
            .capture_live_snapshot()
            .map_err(|_| synchronization_failure())?
    };
    publisher.publish_frame(
        RemoteProjectionFrame {
            snapshot,
            confirmed_through: confirmed_result
                .map(|result| result.final_tick)
                .or_else(|| client.confirmed_tick()),
            confirmed_result,
        },
        rollback_retain_through,
        event_ticks,
        completed_status,
    );
    *last_published_tick = Some(target_tick);
    Ok(())
}

fn projection_source_world() -> World {
    let mut world = World::new();
    world.insert_resource(SimEventJournal::default());
    world.insert_resource(CombatPresentationIntentJournal::default());
    world.insert_resource(FighterPresentationIntentJournal::default());
    world.insert_resource(ItemPresentationIntentJournal::default());
    world.insert_resource(ArenaPresentationIntentJournal::default());
    world.insert_resource(SpecialPresentationIntentJournal::default());
    world.insert_resource(BeePresentationIntentJournal::default());
    world.insert_resource(ChickPresentationIntentJournal::default());
    world.insert_resource(PenguinPresentationIntentJournal::default());
    world
}

fn discard_projection_after(
    source: &mut World,
    projector: &mut LivePresentationProjector,
    retained_through: SimTick,
) {
    source
        .resource_mut::<SimEventJournal>()
        .discard_after(retained_through);
    source
        .resource_mut::<CombatPresentationIntentJournal>()
        .discard_after(retained_through);
    source
        .resource_mut::<FighterPresentationIntentJournal>()
        .discard_after(retained_through);
    source
        .resource_mut::<ItemPresentationIntentJournal>()
        .discard_after(retained_through);
    source
        .resource_mut::<ArenaPresentationIntentJournal>()
        .discard_after(retained_through);
    source
        .resource_mut::<SpecialPresentationIntentJournal>()
        .discard_after(retained_through);
    source
        .resource_mut::<BeePresentationIntentJournal>()
        .discard_after(retained_through);
    source
        .resource_mut::<ChickPresentationIntentJournal>()
        .discard_after(retained_through);
    source
        .resource_mut::<PenguinPresentationIntentJournal>()
        .discard_after(retained_through);
    projector.rollback_hooks().discard_after(retained_through);
}

fn install_presentation_tick(
    source: &mut World,
    events: RemotePresentationTick,
) -> Result<(), RemoteOnlinePresentationError> {
    let mut buffer = TickEventBuffer::new(events.tick);
    for record in &events.events {
        let emitted = buffer
            .emit(record.event.id.source, record.event.kind)
            .map_err(RemoteOnlinePresentationError::Event)?;
        if emitted != record.event.id {
            return Err(RemoteOnlinePresentationError::EventIdentityChanged);
        }
    }
    source.resource_mut::<SimEventJournal>().commit(&buffer);
    for record in events.events {
        if let Some(intent) = record.combat {
            source
                .resource_mut::<CombatPresentationIntentJournal>()
                .record(intent)
                .map_err(RemoteOnlinePresentationError::Event)?;
        }
        if let Some(intent) = record.combat_cue {
            source
                .resource_mut::<CombatPresentationIntentJournal>()
                .record_cue(intent)
                .map_err(RemoteOnlinePresentationError::Event)?;
        }
        if let Some(intent) = record.fighter {
            source
                .resource_mut::<FighterPresentationIntentJournal>()
                .record(intent)
                .map_err(RemoteOnlinePresentationError::Event)?;
        }
        if let Some(intent) = record.item {
            source
                .resource_mut::<ItemPresentationIntentJournal>()
                .record(intent)
                .map_err(RemoteOnlinePresentationError::Event)?;
        }
        if let Some(intent) = record.arena {
            source
                .resource_mut::<ArenaPresentationIntentJournal>()
                .record(intent)
                .map_err(RemoteOnlinePresentationError::Event)?;
        }
        if let Some(intent) = record.special {
            source
                .resource_mut::<SpecialPresentationIntentJournal>()
                .record(intent)
                .map_err(RemoteOnlinePresentationError::Event)?;
        }
        if let Some(intent) = record.bee {
            source
                .resource_mut::<BeePresentationIntentJournal>()
                .record(intent)
                .map_err(RemoteOnlinePresentationError::Event)?;
        }
        if let Some(intent) = record.chick {
            source
                .resource_mut::<ChickPresentationIntentJournal>()
                .record(intent)
                .map_err(RemoteOnlinePresentationError::Event)?;
        }
        if let Some(intent) = record.penguin {
            source
                .resource_mut::<PenguinPresentationIntentJournal>()
                .record(intent)
                .map_err(RemoteOnlinePresentationError::Event)?;
        }
    }
    Ok(())
}

fn map_protocol_build_error(error: ClientProtocolBuildError) -> OnlineFailure {
    match error {
        ClientProtocolBuildError::Protocol(_) => incompatible_failure(),
        ClientProtocolBuildError::Session(error) => OnlineFailure::from_session(error),
        ClientProtocolBuildError::RuntimeConfig(_)
        | ClientProtocolBuildError::RuntimeQueue(_)
        | ClientProtocolBuildError::InvalidResyncTimeout
        | ClientProtocolBuildError::InvalidClockProbeInterval => internal_failure(),
    }
}

fn map_protocol_error(
    error: ClientProtocolError<LiveSimulationError>,
    generation: u64,
    local_confirmed_tick: Option<SimTick>,
) -> RemoteWorkerFailure {
    match error {
        ClientProtocolError::Recoverable(ClientProtocolRecoverableError::OutboundBackpressure) => {
            capacity_failure().into()
        }
        ClientProtocolError::Recoverable(_) => synchronization_failure().into(),
        ClientProtocolError::Fatal(ClientProtocolFatalError::AuthorityDisconnect(message)) => {
            RemoteWorkerFailure::AuthorityDisconnect(RemoteAuthorityDisconnect {
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
        | ClientProtocolFault::UnexpectedMessage => synchronization_failure(),
        ClientProtocolFault::Session => synchronization_failure(),
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

    use crate::authority::{AuthoritySimulation, AuthorityTickReport};
    use crate::authority_input::AuthorityInputConfig;
    use crate::authority_peer_hub::{
        AuthorityAdvanceOutcome, AuthorityPeerHub, AuthorityPeerHubConfig,
    };
    use crate::game_state::{LocalSetup, MatchPhase, MatchState};
    use crate::headless::build_headless_simulation;
    use crate::match_config::{MatchBuildOptions, build_headless_match_config};
    use crate::network_io::InProcessEndpoint;
    use crate::network_protocol::{AuthorityKind, MatchId, ReconnectClaim, StateHash};
    use crate::reconnect::{AuthenticatedPeer, AuthenticatedUserId};

    const TEST_TIMEOUT: Duration = Duration::from_secs(5);

    fn peer() -> PeerId {
        PeerId::new(41).unwrap()
    }

    fn user() -> AuthenticatedUserId {
        AuthenticatedUserId::new(4_100).unwrap()
    }

    fn match_config() -> HeadlessMatchConfig {
        let setup = LocalSetup::default();
        build_headless_match_config(
            &setup,
            MatchBuildOptions::single_peer(
                MatchId::new(*b"remote-client-01").unwrap(),
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
        client: RemoteOnlineClient,
        hub: TestHub,
        authority_network_tick: SimTick,
    }

    impl Harness {
        fn new(command_capacity: usize) -> Self {
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
            let client = RemoteOnlineClient::spawn_manual(
                client_endpoint,
                config.clone(),
                peer(),
                RemoteOnlineClientConfig {
                    command_capacity,
                    ..RemoteOnlineClientConfig::default()
                },
            )
            .unwrap();
            client.mark_content_loaded();
            Self {
                config,
                client,
                hub,
                authority_network_tick: SimTick::ZERO,
            }
        }

        fn round(&mut self, advance_authority: bool) -> Option<AuthorityTickReport> {
            if !self.client.advance_manual(1) {
                if matches!(
                    self.client.terminal(),
                    Some(RemoteOnlineTerminal::Completed(_))
                ) {
                    return None;
                }
                panic!("client terminal during round: {:?}", self.client.terminal());
            }
            self.authority_network_tick = self.authority_network_tick.next();
            self.hub.pump_network(self.authority_network_tick).unwrap();
            let report = if advance_authority {
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
            // Flush control/state queued while handling inbound or stepping.
            self.hub.pump_network(self.authority_network_tick).unwrap();
            report
        }

        fn drive_until_fighting(&mut self) {
            for _ in 0..512 {
                self.round(true);
                if self.client.status().phase == RemoteOnlineClientPhase::Fighting {
                    return;
                }
            }
            panic!("startup did not reach fighting: {:?}", self.client.status());
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

    fn moving_sample() -> RemoteLocalInputBatch {
        RemoteLocalInputBatch::new(&[RemoteLocalInputSample {
            seat: SeatId::new(0).unwrap(),
            movement_x: QuantizedAxis::new(100).unwrap(),
            held_buttons: InputButtons::new(InputButtons::LIGHT).unwrap(),
            pressed_buttons: InputButtons::new(InputButtons::LIGHT).unwrap(),
            ..RemoteLocalInputSample::default()
        }])
        .unwrap()
    }

    #[test]
    fn fake_endpoint_runs_separate_authority_prediction_and_render_worlds_through_correction() {
        let mut harness = Harness::new(64);
        harness.drive_until_fighting();
        assert!(harness.client.status().countdown_start_tick.is_some());

        assert_eq!(
            harness.client.submit_inputs(moving_sample()),
            RemoteCommandSubmitOutcome::Queued
        );
        assert!(harness.client.advance_manual(1));

        // Delay client->authority delivery while the authority commits through
        // the client's prediction frontier. The late relay must correct the
        // independently-owned predicted world, not mutate the authority world.
        let predicted_frontier = harness.client.status().predicted_tick.unwrap();
        while harness.hub.authority().simulation().current_tick() < predicted_frontier {
            let (outcome, _) = harness.hub.try_advance(neutral_disconnected).unwrap();
            assert_eq!(outcome, AuthorityAdvanceOutcome::Advanced);
        }
        harness.authority_network_tick = harness.authority_network_tick.next();
        harness
            .hub
            .pump_network(harness.authority_network_tick)
            .unwrap();
        harness
            .hub
            .pump_network(harness.authority_network_tick)
            .unwrap();
        harness.settle(12);

        assert!(harness.client.status().protocol.rollback_corrections > 0);
        let mut render_world = build_headless_simulation(harness.config.clone()).unwrap();
        let update = harness
            .client
            .project_latest(render_world.world_mut())
            .unwrap();
        let projection = update.projection.expect("prediction published a snapshot");
        assert_eq!(
            render_world.current_sim_tick(),
            harness.client.status().predicted_tick.unwrap()
        );
        assert_eq!(projection.snapshot_tick, render_world.current_sim_tick());
        assert_ne!(
            render_world.world() as *const World,
            harness.hub.authority().simulation().world() as *const World
        );
    }

    #[test]
    fn result_is_verified_published_reliably_and_keeps_worker_alive_until_stop() {
        let mut harness = Harness::new(64);
        harness.drive_until_fighting();
        harness.settle(4);

        harness
            .hub
            .authority_mut()
            .simulation_mut()
            .world_mut()
            .resource_mut::<MatchState>()
            .phase = MatchPhase::Results;
        for _ in 0..64 {
            if harness.client.terminal().is_some() {
                break;
            }
            harness.round(true);
        }

        let result = harness
            .client
            .confirmed_result()
            .expect("verified result retained in mailbox");
        assert_eq!(
            harness.client.terminal(),
            Some(RemoteOnlineTerminal::Completed(result))
        );
        assert_eq!(
            harness.client.status().phase,
            RemoteOnlineClientPhase::Results
        );
        assert_eq!(
            harness.client.status().confirmed_tick,
            Some(result.final_tick)
        );

        let mut render_world = build_headless_simulation(harness.config.clone()).unwrap();
        let update = harness
            .client
            .project_latest(render_world.world_mut())
            .unwrap();
        assert_eq!(update.confirmed_result, Some(result));
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

        let repeated = harness
            .client
            .project_latest(render_world.world_mut())
            .unwrap();
        assert_eq!(repeated.projected_confirmed_result, None);
        assert_eq!(
            render_world
                .world()
                .resource::<ConfirmedProgressionLedger>()
                .len(),
            1
        );

        let before_keepalive = harness.client.metrics();
        assert_eq!(
            harness.client.submit_inputs(moving_sample()),
            RemoteCommandSubmitOutcome::Queued
        );
        for _ in 0..8 {
            assert!(harness.client.advance_manual(1));
            harness.authority_network_tick = harness.authority_network_tick.next();
            harness
                .hub
                .pump_network(harness.authority_network_tick)
                .unwrap();
        }
        let after_keepalive = harness.client.metrics();
        assert!(after_keepalive.worker_iterations >= before_keepalive.worker_iterations + 8);
        assert_eq!(
            after_keepalive.input_ticks_submitted, before_keepalive.input_ticks_submitted,
            "Results keepalive must never resubmit queued gameplay input"
        );
        assert_eq!(
            harness.client.terminal(),
            Some(RemoteOnlineTerminal::Completed(result))
        );
        assert_eq!(
            harness.client.status().phase,
            RemoteOnlineClientPhase::Results
        );

        harness.client.stop();
        assert_eq!(
            harness.client.terminal(),
            Some(RemoteOnlineTerminal::Completed(result))
        );
        assert_eq!(
            harness.client.status().phase,
            RemoteOnlineClientPhase::Results
        );
    }

    #[test]
    fn final_frame_and_completed_terminal_are_one_atomic_mailbox_publication() {
        let config = match_config();
        let snapshot = build_headless_simulation(config)
            .unwrap()
            .capture_live_snapshot()
            .unwrap();
        let result = ConfirmedSessionResult {
            result_id: 44,
            final_tick: snapshot.header.tick,
            final_hash: StateHash(snapshot.canonical_hash().unwrap()),
        };
        let shared_metrics = Arc::new(SharedWorkerMetrics::default());
        let state = Arc::new(Mutex::new(RemoteMailboxState::new(1, false)));
        let (signal_tx, signal_rx) = mpsc::sync_channel(REMOTE_CLIENT_SIGNAL_CAPACITY);
        let publisher = RemoteMailboxPublisher {
            state: Arc::clone(&state),
            signal: signal_tx,
            metrics: shared_metrics,
        };
        let mut inbox = RemoteMailboxInbox {
            state,
            signal: signal_rx,
        };

        publisher.publish_frame(
            RemoteProjectionFrame {
                snapshot,
                confirmed_through: Some(result.final_tick),
                confirmed_result: Some(result),
            },
            None,
            Vec::new(),
            Some(RemoteOnlineClientStatus::default()),
        );
        let completed = inbox.drain();
        assert!(completed.frame.is_some());
        assert_eq!(completed.confirmed_result, Some(result));
        assert_eq!(completed.status.phase, RemoteOnlineClientPhase::Results);
        assert_eq!(
            completed.terminal,
            Some(RemoteOnlineTerminal::Completed(result))
        );
    }

    #[test]
    fn reconnect_reuses_actual_countdown_boundary_and_applies_authority_snapshot() {
        let mut harness = Harness::new(64);
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
        harness.client.reconnect_manual(client_endpoint).unwrap();
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
        assert_eq!(after.authority_disconnect, None);
        assert_eq!(harness.hub.metrics().reconnects_completed, 1);
    }

    #[test]
    fn typed_authority_disconnect_is_published_atomically_with_local_progress() {
        use crate::network_protocol::{DisconnectCode, RetryDisposition};

        let mut harness = Harness::new(64);
        harness.drive_until_fighting();
        harness.settle(4);
        let before = harness.client.status();
        let connection = harness.hub.connection_for_peer(peer()).unwrap();
        harness.hub.revoke_authentication(connection).unwrap();

        for _ in 0..16 {
            harness.authority_network_tick = harness.authority_network_tick.next();
            harness
                .hub
                .pump_network(harness.authority_network_tick)
                .unwrap();
            let _ = harness.client.advance_manual(1);
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
        let status = harness.client.status();
        assert_eq!(disconnect.generation, harness.client.generation());
        assert_eq!(disconnect.local_confirmed_tick, before.confirmed_tick);
        assert_eq!(
            disconnect.message.match_id,
            Some(harness.config.manifest.match_id)
        );
        assert_eq!(
            disconnect.message.code,
            DisconnectCode::AuthenticationFailed
        );
        assert_eq!(disconnect.message.retry, RetryDisposition::Fatal);
        assert_eq!(disconnect.message.detail_code, 13);
        assert_eq!(status.authority_disconnect, Some(disconnect));
        assert_eq!(
            status.failure,
            Some(OnlineFailure::from_disconnect(disconnect.message))
        );
        assert_eq!(status.phase, RemoteOnlineClientPhase::Failed);
    }

    #[test]
    fn command_ingress_is_bounded_and_clean_shutdown_is_terminal() {
        let mut harness = Harness::new(1);
        let mut observed_full = false;
        for _ in 0..10_000 {
            match harness.client.submit_inputs(moving_sample()) {
                RemoteCommandSubmitOutcome::Full => {
                    observed_full = true;
                    break;
                }
                RemoteCommandSubmitOutcome::Queued => {}
                RemoteCommandSubmitOutcome::Disconnected => panic!("worker disconnected"),
            }
        }
        assert!(observed_full);
        assert!(harness.client.metrics().command_queue_high_water <= 1);
        assert!(harness.client.metrics().command_queue_full > 0);

        harness.client.stop();
        assert_eq!(
            harness.client.terminal(),
            Some(RemoteOnlineTerminal::Stopped)
        );
        assert_eq!(
            harness.client.status().phase,
            RemoteOnlineClientPhase::Stopped
        );
        assert_eq!(
            harness.client.submit_inputs(moving_sample()),
            RemoteCommandSubmitOutcome::Disconnected
        );
    }

    #[test]
    fn dropped_transport_fails_closed_with_stable_localizable_failure() {
        let config = match_config();
        let (client_endpoint, authority_endpoint) = InProcessEndpoint::pair(8).unwrap();
        drop(authority_endpoint);
        let client = RemoteOnlineClient::spawn_manual(
            client_endpoint,
            config,
            peer(),
            RemoteOnlineClientConfig::default(),
        )
        .unwrap();
        let _ = client.advance_manual(1);
        let deadline = Instant::now() + TEST_TIMEOUT;
        while client.terminal().is_none() && Instant::now() < deadline {
            let _ = client.advance_manual(1);
        }
        let Some(RemoteOnlineTerminal::Failed(failure)) = client.terminal() else {
            panic!("transport failure was not published: {:?}", client.status());
        };
        assert_eq!(failure.code, OnlineFailureCode::ConnectionTimedOut);
        assert_eq!(failure.recovery, OnlineRecoveryAction::Reconnect);
        assert_eq!(failure.message_key(), "online.error.connection_timeout");
        assert_eq!(client.status().phase, RemoteOnlineClientPhase::Failed);
    }
}
