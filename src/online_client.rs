//! Shipping client composition over the shared authority/protocol boundary.
//!
//! `EmbeddedOnlineMatch` is the offline/listen-host form of the production
//! online client. It intentionally serializes every message through the same
//! runtime codec as UDP/Steam, predicts in a separate render-free world, and
//! projects only settled snapshots/events into the Bevy rendering world.

use bevy::prelude::*;
use std::cell::Cell;
use std::collections::VecDeque;
use std::error::Error;
use std::fmt;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::arena::{ArenaPresentationIntent, ArenaPresentationIntentJournal};
use crate::authority::AuthorityTickReport;
use crate::authority_input::AuthorityInputConfig;
use crate::authority_thread::{AuthorityThreadConfig, AuthorityThreadConfigError, SixtyHzSchedule};
use crate::bee_skills::{BeePresentationIntent, BeePresentationIntentJournal};
use crate::chick_skills::{ChickPresentationIntent, ChickPresentationIntentJournal};
use crate::combat::{
    CombatPresentationCueIntent, CombatPresentationIntent, CombatPresentationIntentJournal,
};
use crate::confirmed_progression::{ConfirmedProgressionError, ConfirmedProgressionLedger};
use crate::fighter::{FighterPresentationIntent, FighterPresentationIntentJournal};
use crate::headless::{
    HeadlessBuildError, HeadlessMatchConfig, build_headless_simulation, build_predicted_simulation,
};
use crate::items::{ItemPresentationIntent, ItemPresentationIntentJournal};
use crate::live_authority::{LiveSimulationDriver, LiveSimulationError};
use crate::live_input::local_tick_to_network_input;
use crate::local_loopback::{LocalLoopbackConfig, LocalLoopbackError, LocalLoopbackMatch};
use crate::match_config::{MatchBuildOptions, MatchConfigError, build_headless_match_config};
use crate::match_presentation::ConfirmedMatchPresentation;
use crate::network_protocol::{
    AuthorityKind, InputFrame, InputSequence, MAX_SEATS, MatchId, MatchManifest, PeerId, SeatId,
    SeatOwner, SimTick,
};
use crate::online_failure::{
    OnlineFailure, OnlineFailureCode, OnlineFailureSeverity, OnlineRecoveryAction,
};
use crate::penguin_skills::{PenguinPresentationIntent, PenguinPresentationIntentJournal};
use crate::predicted_client::{PredictedClient, PredictedClientError};
use crate::presentation_projection::{
    LivePresentationProjectionError, LivePresentationProjectionReport, LivePresentationProjector,
    LiveRollbackPresentationHooks,
};
use crate::rollback::{InstantRollbackTiming, RollbackEventDiscard};
use crate::session::ConfirmedSessionResult;
use crate::sim_event::{
    EventEmitError, MAX_SIM_EVENTS_PER_TICK, SIM_EVENT_HISTORY_TICKS, SimEvent, SimEventJournal,
    TickEventBuffer,
};
use crate::snapshot::CanonicalSnapshot;
use crate::specials::{SpecialPresentationIntent, SpecialPresentationIntentJournal};
use crate::tick_input::{LocalSeatId, LocalTickInputState};
use crate::{game_state, simulation, user_mode};

pub type LivePredictedClient =
    PredictedClient<LiveSimulationDriver, LiveRollbackPresentationHooks, InstantRollbackTiming>;

pub type EmbeddedLiveLoopback = LocalLoopbackMatch<LiveSimulationDriver, LivePredictedClient>;

type ThreadedPredictedClient =
    PredictedClient<LiveSimulationDriver, ThreadRollbackHooks, InstantRollbackTiming>;
type ThreadedLiveLoopback = LocalLoopbackMatch<LiveSimulationDriver, ThreadedPredictedClient>;

pub type EmbeddedLoopbackError =
    LocalLoopbackError<LiveSimulationError, PredictedClientError<LiveSimulationError>>;

#[derive(Debug)]
pub enum EmbeddedOnlineMatchError {
    MatchConfig(MatchConfigError),
    AuthorityBuild(HeadlessBuildError),
    PredictionBuild(HeadlessBuildError),
    Prediction(PredictedClientError<LiveSimulationError>),
    Loopback(EmbeddedLoopbackError),
    Projection(LivePresentationProjectionError),
    ConfirmedProgression(ConfirmedProgressionError),
    AuthorityThreadConfig(AuthorityThreadConfigError),
    AuthorityThreadSpawn(std::io::Error),
    AuthorityThreadBootstrap(String),
    AuthorityThreadDisconnected,
    AuthorityThreadFailed(String),
    TimelineExhausted,
    InvalidLocalSeat(u8),
    NoLocalHumanSeat,
}

impl fmt::Display for EmbeddedOnlineMatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "embedded online match failed: {self:?}")
    }
}

impl Error for EmbeddedOnlineMatchError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::MatchConfig(error) => Some(error),
            Self::AuthorityBuild(error) | Self::PredictionBuild(error) => Some(error),
            Self::Prediction(error) => Some(error),
            Self::Loopback(error) => Some(error),
            Self::Projection(error) => Some(error),
            Self::ConfirmedProgression(error) => Some(error),
            Self::AuthorityThreadConfig(error) => Some(error),
            Self::AuthorityThreadSpawn(error) => Some(error),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EmbeddedOnlineClientPhase {
    #[default]
    Idle,
    Starting,
    Fighting,
    Results,
    Failed,
}

/// Read-only UI/diagnostic projection of the non-send embedded client runtime.
#[derive(Resource, Clone, Debug, Default, PartialEq, Eq)]
pub struct EmbeddedOnlineClientStatus {
    pub phase: EmbeddedOnlineClientPhase,
    pub latest_tick: Option<SimTick>,
    /// Stable, localizable player-facing failure. Internal worker strings
    /// remain diagnostic-only and never cross this UI contract.
    pub failure: Option<OnlineFailure>,
}

/// Main-thread owner for nested Bevy simulation worlds.
///
/// `bevy::App` is intentionally not `Send`, so this value is installed as a
/// non-send resource and driven only by exclusive main-thread systems.
#[derive(Default)]
pub struct EmbeddedOnlineClientController {
    active: Option<ThreadedEmbeddedOnlineMatch>,
    /// True only while this controller owns the rendered world as an online
    /// projection target. Offline idle reconciliation must not infer
    /// ownership from the absence of a request: doing so would clear ordinary
    /// local canonical entities every frame.
    owns_projection_target: bool,
    session_counter: u64,
    failed_seed: Option<u64>,
    started_revision: Option<u64>,
    failed_revision: Option<u64>,
    #[cfg(test)]
    injected_startup_failure: bool,
    #[cfg(test)]
    injected_runtime_failure: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EmbeddedOnlineTickReport {
    pub authority: AuthorityTickReport,
    pub presentation: LivePresentationProjectionReport,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ThreadedEmbeddedOnlineMetrics {
    pub input_queue_full: u64,
    pub dropped_presentation_event_ticks: u64,
}

/// Worker-local rollback hook. It is intentionally `!Send`: it is constructed
/// beside the predicted Bevy world, used only on the authority worker, and
/// converted into an owned tick notification before crossing threads.
#[derive(Clone, Debug, Default)]
struct ThreadRollbackHooks {
    pending_retain_through: Rc<Cell<Option<SimTick>>>,
}

impl ThreadRollbackHooks {
    fn take(&self) -> Option<SimTick> {
        self.pending_retain_through.take()
    }
}

impl RollbackEventDiscard for ThreadRollbackHooks {
    fn discard_after(&mut self, retained_through: SimTick) {
        let retained_through = self
            .pending_retain_through
            .get()
            .map_or(retained_through, |pending| pending.min(retained_through));
        self.pending_retain_through.set(Some(retained_through));
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ThreadPresentationEvent {
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
struct ThreadPresentationTick {
    tick: SimTick,
    events: Vec<ThreadPresentationEvent>,
}

impl ThreadPresentationTick {
    fn capture(source: &World, tick: SimTick) -> Result<Option<Self>, String> {
        let journal = source
            .get_resource::<SimEventJournal>()
            .ok_or_else(|| "predicted worker is missing SimEventJournal".to_owned())?;
        let Some(events) = journal.events_at(tick) else {
            return Ok(None);
        };
        if events.len() > MAX_SIM_EVENTS_PER_TICK {
            return Err("predicted event journal exceeded its fixed tick bound".to_owned());
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
            .map(|event| ThreadPresentationEvent {
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
struct ThreadProjectionSnapshot {
    snapshot: CanonicalSnapshot,
    confirmed_through: Option<SimTick>,
    authority: Option<AuthorityTickReport>,
    confirmed_result: Option<ConfirmedSessionResult>,
}

#[derive(Default)]
struct ThreadProjectionMailboxState {
    snapshot: Option<ThreadProjectionSnapshot>,
    event_ticks: VecDeque<ThreadPresentationTick>,
    rollback_retain_through: Option<SimTick>,
    dropped_event_ticks: u64,
    /// Reliable latest result slot. Unlike cosmetic event ticks, this is never
    /// drained or evicted; repeated observations are idempotent by result ID.
    confirmed_result: Option<ConfirmedSessionResult>,
    terminal: Option<Result<(), String>>,
}

impl ThreadProjectionMailboxState {
    fn retain_events_through(&mut self, retained_through: SimTick) {
        self.event_ticks
            .retain(|events| events.tick <= retained_through);
        self.rollback_retain_through = Some(
            self.rollback_retain_through
                .map_or(retained_through, |pending| pending.min(retained_through)),
        );
    }

    fn push_event_tick(&mut self, events: ThreadPresentationTick) {
        if let Some(existing) = self
            .event_ticks
            .iter_mut()
            .find(|queued| queued.tick == events.tick)
        {
            *existing = events;
        } else {
            self.event_ticks.push_back(events);
        }
        while self.event_ticks.len() > SIM_EVENT_HISTORY_TICKS {
            self.event_ticks.pop_front();
            self.dropped_event_ticks = self.dropped_event_ticks.saturating_add(1);
        }
    }

    fn observe_confirmed_result(&mut self, result: ConfirmedSessionResult) {
        // Session validation guarantees a match has one immutable result. Keep
        // the first accepted value so repeated reliable delivery is idempotent.
        self.confirmed_result.get_or_insert(result);
    }
}

struct ThreadProjectionPublisher {
    state: Arc<Mutex<ThreadProjectionMailboxState>>,
    signal: SyncSender<()>,
}

impl ThreadProjectionPublisher {
    fn publish(
        &self,
        snapshot: ThreadProjectionSnapshot,
        rollback_retain_through: Option<SimTick>,
        event_ticks: Vec<ThreadPresentationTick>,
    ) {
        {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            if let Some(retained_through) = rollback_retain_through {
                state.retain_events_through(retained_through);
            }
            for events in event_ticks {
                state.push_event_tick(events);
            }
            if let Some(result) = snapshot.confirmed_result {
                state.observe_confirmed_result(result);
            }
            state.snapshot = Some(snapshot);
        }
        let _ = self.signal.try_send(());
    }

    fn finish(&self, result: Result<(), String>) {
        {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            state.terminal = Some(result);
        }
        let _ = self.signal.try_send(());
    }
}

struct ThreadProjectionDrain {
    snapshot: Option<ThreadProjectionSnapshot>,
    event_ticks: Vec<ThreadPresentationTick>,
    rollback_retain_through: Option<SimTick>,
    terminal: Option<Result<(), String>>,
    dropped_event_ticks: u64,
    confirmed_result: Option<ConfirmedSessionResult>,
}

struct ThreadProjectionInbox {
    state: Arc<Mutex<ThreadProjectionMailboxState>>,
    signal: Receiver<()>,
}

impl ThreadProjectionInbox {
    fn drain(&mut self) -> ThreadProjectionDrain {
        while self.signal.try_recv().is_ok() {}
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        ThreadProjectionDrain {
            snapshot: state.snapshot.take(),
            event_ticks: state.event_ticks.drain(..).collect(),
            rollback_retain_through: state.rollback_retain_through.take(),
            terminal: state.terminal.clone(),
            dropped_event_ticks: state.dropped_event_ticks,
            confirmed_result: state.confirmed_result,
        }
    }

    fn wait_for_initial_snapshot(
        &mut self,
        timeout: Duration,
    ) -> Result<(), EmbeddedOnlineMatchError> {
        let deadline = Instant::now().checked_add(timeout);
        loop {
            {
                let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
                if state.snapshot.is_some() {
                    return Ok(());
                }
                if let Some(Err(error)) = &state.terminal {
                    return Err(EmbeddedOnlineMatchError::AuthorityThreadBootstrap(
                        error.clone(),
                    ));
                }
            }
            let remaining = deadline
                .map(|deadline| deadline.saturating_duration_since(Instant::now()))
                .unwrap_or(timeout);
            match self.signal.recv_timeout(remaining) {
                Ok(()) => {}
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {
                    return Err(EmbeddedOnlineMatchError::AuthorityThreadBootstrap(
                        "timed out before the worker published its initial synchronized snapshot"
                            .to_owned(),
                    ));
                }
            }
        }
    }

    #[cfg(test)]
    fn wait_until_tick(
        &mut self,
        expected: SimTick,
        timeout: Duration,
    ) -> Result<(), EmbeddedOnlineMatchError> {
        let deadline = Instant::now().checked_add(timeout);
        loop {
            {
                let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
                if state
                    .snapshot
                    .as_ref()
                    .is_some_and(|snapshot| snapshot.snapshot.header.tick >= expected)
                {
                    return Ok(());
                }
                if let Some(Err(error)) = &state.terminal {
                    return Err(EmbeddedOnlineMatchError::AuthorityThreadFailed(
                        error.clone(),
                    ));
                }
            }
            let remaining = deadline
                .map(|deadline| deadline.saturating_duration_since(Instant::now()))
                .unwrap_or(timeout);
            match self.signal.recv_timeout(remaining) {
                Ok(()) => {}
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {
                    return Err(EmbeddedOnlineMatchError::AuthorityThreadDisconnected);
                }
            }
        }
    }
}

/// One local/listen-host match using the production three-world topology.
pub struct EmbeddedOnlineMatch {
    peer_id: PeerId,
    runner: EmbeddedLiveLoopback,
    projector: LivePresentationProjector,
}

impl EmbeddedOnlineMatch {
    pub fn new(
        config: HeadlessMatchConfig,
        peer_id: PeerId,
        input_config: AuthorityInputConfig,
        loopback_config: LocalLoopbackConfig,
    ) -> Result<Self, EmbeddedOnlineMatchError> {
        let manifest = config.manifest;
        let authority = build_headless_simulation(config.clone())
            .map_err(EmbeddedOnlineMatchError::AuthorityBuild)?;
        let predicted = build_predicted_simulation(config)
            .map_err(EmbeddedOnlineMatchError::PredictionBuild)?;
        let projector = LivePresentationProjector::new();
        let predicted = PredictedClient::with_hooks(
            predicted,
            manifest.match_id,
            usize::from(manifest.snapshot_history_ticks),
            projector.rollback_hooks(),
            InstantRollbackTiming::default(),
        )
        .map_err(EmbeddedOnlineMatchError::Prediction)?;
        let mut runner = LocalLoopbackMatch::with_client_world(
            manifest,
            peer_id,
            authority,
            predicted,
            input_config,
            loopback_config,
        )
        .map_err(EmbeddedOnlineMatchError::Loopback)?;
        runner.start().map_err(EmbeddedOnlineMatchError::Loopback)?;

        Ok(Self {
            peer_id,
            runner,
            projector,
        })
    }

    pub const fn runner(&self) -> &EmbeddedLiveLoopback {
        &self.runner
    }

    pub fn runner_mut(&mut self) -> &mut EmbeddedLiveLoopback {
        &mut self.runner
    }

    pub const fn projector(&self) -> &LivePresentationProjector {
        &self.projector
    }

    /// Prepares an already-started rendered app and applies the initial network
    /// snapshot before the first playable tick.
    pub fn prepare_presentation_target(
        &mut self,
        target: &mut World,
    ) -> Result<LivePresentationProjectionReport, EmbeddedOnlineMatchError> {
        if !target.contains_resource::<ConfirmedProgressionLedger>() {
            target.insert_resource(ConfirmedProgressionLedger::default());
        }
        self.projector
            .prepare_target(target, self.runner.manifest())
            .map_err(EmbeddedOnlineMatchError::Projection)?;
        self.project_settled_client(target)
    }

    /// Drains one fixed input frame per locally owned seat, predicts the tick,
    /// advances the serialized loopback authority, reconciles all returned
    /// state, and finally projects the settled client state for rendering.
    pub fn tick(
        &mut self,
        local_inputs: &mut LocalTickInputState,
        target: &mut World,
    ) -> Result<EmbeddedOnlineTickReport, EmbeddedOnlineMatchError> {
        let tick = self
            .runner
            .client_world()
            .predicted_tick()
            .ok_or(EmbeddedOnlineMatchError::TimelineExhausted)?
            .0
            .checked_add(1)
            .map(SimTick)
            .ok_or(EmbeddedOnlineMatchError::TimelineExhausted)?;

        let mut prediction = [None; MAX_SEATS];
        let mut local_frames = Vec::with_capacity(MAX_SEATS);
        for assignment in self.runner.manifest().ownership.as_slice() {
            if assignment.owner != SeatOwner::Peer(self.peer_id) {
                continue;
            }
            let local_seat = LocalSeatId::new(usize::from(assignment.seat.get())).ok_or(
                EmbeddedOnlineMatchError::InvalidLocalSeat(assignment.seat.get()),
            )?;
            let raw = local_inputs.drain_for_tick(local_seat, tick.get());
            let frame = local_tick_to_network_input(raw, local_inputs.gestures_mut(local_seat));
            prediction[usize::from(assignment.seat.get())] = Some(frame);
            local_frames.push(frame);
        }
        if local_frames.is_empty() {
            return Err(EmbeddedOnlineMatchError::NoLocalHumanSeat);
        }

        self.runner
            .client_world_mut()
            .predict_next(prediction)
            .map_err(EmbeddedOnlineMatchError::Prediction)?;
        let authority = self
            .runner
            .run_local_tick(&local_frames)
            .map_err(EmbeddedOnlineMatchError::Loopback)?;
        let presentation = self.project_settled_client(target)?;

        Ok(EmbeddedOnlineTickReport {
            authority,
            presentation,
        })
    }

    /// Installs one settled predicted frame and, when terminal, binds the
    /// reliable result to that exact frame before exposing either to render
    /// presentation. This mirrors the threaded/native client transaction and
    /// prevents a synchronous embedded caller from observing Results while the
    /// render world still contains a pre-result snapshot.
    fn project_settled_client(
        &mut self,
        target: &mut World,
    ) -> Result<LivePresentationProjectionReport, EmbeddedOnlineMatchError> {
        let confirmed_result = self.runner.confirmed_result();
        let snapshot = self
            .runner
            .client_world()
            .world()
            .capture_live_snapshot()
            .map_err(|error| {
                EmbeddedOnlineMatchError::Projection(
                    LivePresentationProjectionError::SourceSnapshot(error),
                )
            })?;
        let prepared = if let Some(confirmed) = confirmed_result {
            Some(
                target
                    .resource_mut::<ConfirmedProgressionLedger>()
                    .prepare_observation(self.runner.manifest(), confirmed, &snapshot, true)
                    .map_err(EmbeddedOnlineMatchError::ConfirmedProgression)?,
            )
        } else {
            None
        };
        let typed_result = prepared.as_ref().map(|prepared| {
            ConfirmedMatchPresentation::from_confirmed_record(
                self.runner.manifest(),
                self.peer_id,
                prepared.record(),
            )
        });
        let client = self.runner.client_world();
        let report = self
            .projector
            .project_snapshot(
                client.world().world(),
                &snapshot,
                target,
                client.confirmed_tick(),
            )
            .map_err(EmbeddedOnlineMatchError::Projection)?;
        if let Some(prepared) = prepared {
            target
                .resource_mut::<ConfirmedProgressionLedger>()
                .commit_prepared(prepared);
            target.insert_resource(
                typed_result.expect("a prepared embedded result has typed presentation"),
            );
        }
        Ok(report)
    }
}

const EMBEDDED_AUTHORITY_SIGNAL_CAPACITY: usize = 1;
const EMBEDDED_AUTHORITY_BOOTSTRAP_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, Debug)]
enum EmbeddedAuthorityCommand {
    InputSamples([Option<InputFrame>; MAX_SEATS]),
    #[cfg(test)]
    AdvanceManual(u16),
    Stop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EmbeddedAuthorityClockMode {
    Realtime,
    #[cfg(test)]
    Manual,
}

#[derive(Clone, Copy, Debug, Default)]
struct WorkerSeatInput {
    latest: Option<InputFrame>,
    next_sequence: InputSequence,
}

impl WorkerSeatInput {
    fn merge(&mut self, sample: InputFrame) {
        self.latest = Some(match self.latest {
            Some(mut pending) => {
                pending.movement_x = sample.movement_x;
                pending.movement_y = sample.movement_y;
                pending.held_buttons = sample.held_buttons;
                pending.pressed_buttons = crate::network_protocol::InputButtons::new(
                    pending.pressed_buttons.bits() | sample.pressed_buttons.bits(),
                )
                .expect("merging supported button masks stays supported");
                pending.released_buttons = crate::network_protocol::InputButtons::new(
                    pending.released_buttons.bits() | sample.released_buttons.bits(),
                )
                .expect("merging supported button masks stays supported");
                pending
            }
            None => sample,
        });
    }

    fn frame_for_tick(&mut self, tick: SimTick, seat: SeatId) -> InputFrame {
        let mut frame = self.latest.unwrap_or(InputFrame {
            seat,
            ..InputFrame::default()
        });
        frame.tick = tick;
        frame.seat = seat;
        frame.sequence = self.next_sequence;
        self.next_sequence = InputSequence(self.next_sequence.0.wrapping_add(1));
        if let Some(latest) = self.latest.as_mut() {
            latest.pressed_buttons = Default::default();
            latest.released_buttons = Default::default();
        }
        frame
    }
}

/// Listen/offline authority worker. The complete serialized loopback runtime,
/// canonical authority world, and prediction world are born and remain on this
/// thread. The render thread receives only a bounded projection mailbox.
pub struct ThreadedEmbeddedOnlineMatch {
    manifest: MatchManifest,
    peer_id: PeerId,
    commands: SyncSender<EmbeddedAuthorityCommand>,
    force_shutdown: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    inbox: ThreadProjectionInbox,
    projector: LivePresentationProjector,
    projection_source: World,
    pending_samples: [Option<InputFrame>; MAX_SEATS],
    sample_tick: u64,
    latest_authority: Option<AuthorityTickReport>,
    confirmed_result: Option<ConfirmedSessionResult>,
    metrics: ThreadedEmbeddedOnlineMetrics,
}

impl ThreadedEmbeddedOnlineMatch {
    pub fn new(
        config: HeadlessMatchConfig,
        peer_id: PeerId,
        input_config: AuthorityInputConfig,
        loopback_config: LocalLoopbackConfig,
        thread_config: AuthorityThreadConfig,
    ) -> Result<Self, EmbeddedOnlineMatchError> {
        Self::new_with_clock(
            config,
            peer_id,
            input_config,
            loopback_config,
            thread_config,
            EmbeddedAuthorityClockMode::Realtime,
        )
    }

    fn new_with_clock(
        config: HeadlessMatchConfig,
        peer_id: PeerId,
        input_config: AuthorityInputConfig,
        loopback_config: LocalLoopbackConfig,
        thread_config: AuthorityThreadConfig,
        clock: EmbeddedAuthorityClockMode,
    ) -> Result<Self, EmbeddedOnlineMatchError> {
        thread_config
            .validate()
            .map_err(EmbeddedOnlineMatchError::AuthorityThreadConfig)?;
        let manifest = config.manifest;
        if manifest.tick_rate_hz != crate::authority_thread::AUTHORITY_THREAD_TICK_RATE_HZ as u16 {
            return Err(EmbeddedOnlineMatchError::AuthorityThreadBootstrap(format!(
                "manifest tick rate {} Hz does not match the 60 Hz embedded authority clock",
                manifest.tick_rate_hz
            )));
        }

        let (command_tx, command_rx) = mpsc::sync_channel(thread_config.command_capacity);
        let mailbox = Arc::new(Mutex::new(ThreadProjectionMailboxState::default()));
        let (signal_tx, signal_rx) = mpsc::sync_channel(EMBEDDED_AUTHORITY_SIGNAL_CAPACITY);
        let publisher = ThreadProjectionPublisher {
            state: Arc::clone(&mailbox),
            signal: signal_tx,
        };
        let force_shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = Arc::clone(&force_shutdown);
        let worker_manifest = manifest;
        let join = thread::Builder::new()
            .name("afc-embedded-authority-60hz".to_owned())
            .spawn(move || {
                run_embedded_authority_worker(
                    config,
                    worker_manifest,
                    peer_id,
                    input_config,
                    loopback_config,
                    thread_config,
                    clock,
                    command_rx,
                    &worker_shutdown,
                    publisher,
                );
            })
            .map_err(EmbeddedOnlineMatchError::AuthorityThreadSpawn)?;

        let mut projection_source = World::new();
        projection_source.insert_resource(SimEventJournal::default());
        projection_source.insert_resource(CombatPresentationIntentJournal::default());
        projection_source.insert_resource(FighterPresentationIntentJournal::default());
        projection_source.insert_resource(ItemPresentationIntentJournal::default());
        projection_source.insert_resource(ArenaPresentationIntentJournal::default());
        projection_source.insert_resource(SpecialPresentationIntentJournal::default());
        projection_source.insert_resource(BeePresentationIntentJournal::default());
        projection_source.insert_resource(ChickPresentationIntentJournal::default());
        projection_source.insert_resource(PenguinPresentationIntentJournal::default());

        let mut online = Self {
            manifest,
            peer_id,
            commands: command_tx,
            force_shutdown,
            join: Some(join),
            inbox: ThreadProjectionInbox {
                state: mailbox,
                signal: signal_rx,
            },
            projector: LivePresentationProjector::new(),
            projection_source,
            pending_samples: [None; MAX_SEATS],
            sample_tick: 0,
            latest_authority: None,
            confirmed_result: None,
            metrics: ThreadedEmbeddedOnlineMetrics::default(),
        };
        online
            .inbox
            .wait_for_initial_snapshot(EMBEDDED_AUTHORITY_BOOTSTRAP_TIMEOUT)?;
        Ok(online)
    }

    pub const fn manifest(&self) -> &MatchManifest {
        &self.manifest
    }

    pub const fn latest_authority(&self) -> Option<AuthorityTickReport> {
        self.latest_authority
    }

    pub const fn confirmed_result(&self) -> Option<ConfirmedSessionResult> {
        self.confirmed_result
    }

    pub const fn metrics(&self) -> ThreadedEmbeddedOnlineMetrics {
        self.metrics
    }

    pub fn prepare_presentation_target(
        &mut self,
        target: &mut World,
    ) -> Result<LivePresentationProjectionReport, EmbeddedOnlineMatchError> {
        if !target.contains_resource::<ConfirmedProgressionLedger>() {
            target.insert_resource(ConfirmedProgressionLedger::default());
        }
        self.projector
            .prepare_target(target, &self.manifest)
            .map_err(EmbeddedOnlineMatchError::Projection)?;
        self.project_latest(target)?.ok_or_else(|| {
            EmbeddedOnlineMatchError::AuthorityThreadBootstrap(
                "worker initial projection disappeared before installation".to_owned(),
            )
        })
    }

    /// Samples device state without advancing authority, then consumes the
    /// newest independently-produced projection. A full input queue coalesces
    /// edges locally and retries; it never blocks rendering or drops a tap.
    pub fn service(
        &mut self,
        local_inputs: &mut LocalTickInputState,
        target: &mut World,
    ) -> Result<Option<EmbeddedOnlineTickReport>, EmbeddedOnlineMatchError> {
        // Consume terminal/result publication before touching ingress. A match
        // may finish between render frames and close its command receiver; the
        // final authoritative projection must win that race rather than being
        // mistaken for an unexpected disconnect.
        let presentation = self.project_latest(target)?;
        if self.confirmed_result.is_none() {
            let fixed_step_tick = self
                .sample_tick
                .checked_add(1)
                .ok_or(EmbeddedOnlineMatchError::TimelineExhausted)?;
            let authority_deadline_tick = self
                .latest_authority
                .and_then(|report| report.tick.0.checked_add(1))
                .unwrap_or(fixed_step_tick);
            // FixedUpdate calls make this counter independent of render FPS.
            // After a long render stall, the worker's observed deadline jumps
            // it forward so gesture windows age in authority ticks rather than
            // accidentally treating the stall as zero elapsed gameplay time.
            self.sample_tick = fixed_step_tick.max(authority_deadline_tick);
            let mut sampled_any = false;
            for assignment in self.manifest.ownership.as_slice() {
                if assignment.owner != SeatOwner::Peer(self.peer_id) {
                    continue;
                }
                let local_seat = LocalSeatId::new(usize::from(assignment.seat.get())).ok_or(
                    EmbeddedOnlineMatchError::InvalidLocalSeat(assignment.seat.get()),
                )?;
                let raw = local_inputs.drain_for_tick(local_seat, self.sample_tick);
                let frame = local_tick_to_network_input(raw, local_inputs.gestures_mut(local_seat));
                merge_pending_sample(
                    &mut self.pending_samples[usize::from(assignment.seat.get())],
                    frame,
                );
                sampled_any = true;
            }
            if !sampled_any {
                return Err(EmbeddedOnlineMatchError::NoLocalHumanSeat);
            }
            self.try_flush_samples()?;
        }

        let Some(presentation) = presentation else {
            return Ok(None);
        };
        Ok(self
            .latest_authority
            .map(|authority| EmbeddedOnlineTickReport {
                authority,
                presentation,
            }))
    }

    fn try_flush_samples(&mut self) -> Result<(), EmbeddedOnlineMatchError> {
        if self.pending_samples.iter().all(Option::is_none) {
            return Ok(());
        }
        match self
            .commands
            .try_send(EmbeddedAuthorityCommand::InputSamples(self.pending_samples))
        {
            Ok(()) => {
                self.pending_samples = [None; MAX_SEATS];
                Ok(())
            }
            Err(TrySendError::Full(_)) => {
                self.metrics.input_queue_full = self.metrics.input_queue_full.saturating_add(1);
                Ok(())
            }
            Err(TrySendError::Disconnected(_)) if self.confirmed_result.is_some() => Ok(()),
            Err(TrySendError::Disconnected(_)) => {
                Err(EmbeddedOnlineMatchError::AuthorityThreadDisconnected)
            }
        }
    }

    fn project_latest(
        &mut self,
        target: &mut World,
    ) -> Result<Option<LivePresentationProjectionReport>, EmbeddedOnlineMatchError> {
        let drain = self.inbox.drain();
        self.metrics.dropped_presentation_event_ticks = drain.dropped_event_ticks;
        if let Some(Err(error)) = drain.terminal.as_ref() {
            return Err(EmbeddedOnlineMatchError::AuthorityThreadFailed(
                error.clone(),
            ));
        }
        if let Some(retained_through) = drain.rollback_retain_through {
            self.projection_source
                .resource_mut::<SimEventJournal>()
                .discard_after(retained_through);
            self.projection_source
                .resource_mut::<CombatPresentationIntentJournal>()
                .discard_after(retained_through);
            self.projection_source
                .resource_mut::<FighterPresentationIntentJournal>()
                .discard_after(retained_through);
            self.projection_source
                .resource_mut::<ItemPresentationIntentJournal>()
                .discard_after(retained_through);
            self.projection_source
                .resource_mut::<ArenaPresentationIntentJournal>()
                .discard_after(retained_through);
            self.projection_source
                .resource_mut::<SpecialPresentationIntentJournal>()
                .discard_after(retained_through);
            self.projection_source
                .resource_mut::<BeePresentationIntentJournal>()
                .discard_after(retained_through);
            self.projection_source
                .resource_mut::<ChickPresentationIntentJournal>()
                .discard_after(retained_through);
            self.projection_source
                .resource_mut::<PenguinPresentationIntentJournal>()
                .discard_after(retained_through);
            let mut hook = self.projector.rollback_hooks();
            hook.discard_after(retained_through);
        }
        for events in drain.event_ticks {
            install_presentation_tick(&mut self.projection_source, events)?;
        }

        let Some(snapshot) = drain.snapshot else {
            return Ok(None);
        };
        debug_assert!(
            snapshot.confirmed_result.is_none()
                || drain.confirmed_result == snapshot.confirmed_result,
            "the reliable result slot must agree with its atomically published final frame"
        );
        let prepared = if let Some(confirmed) = snapshot.confirmed_result {
            Some(
                target
                    .resource_mut::<ConfirmedProgressionLedger>()
                    .prepare_observation(&self.manifest, confirmed, &snapshot.snapshot, true)
                    .map_err(EmbeddedOnlineMatchError::ConfirmedProgression)?,
            )
        } else {
            None
        };
        let presentation = prepared.as_ref().map(|prepared| {
            ConfirmedMatchPresentation::from_confirmed_record(
                &self.manifest,
                self.peer_id,
                prepared.record(),
            )
        });
        self.latest_authority = snapshot.authority.or(self.latest_authority);
        let report = self
            .projector
            .project_snapshot(
                &self.projection_source,
                &snapshot.snapshot,
                target,
                snapshot.confirmed_through,
            )
            .map_err(EmbeddedOnlineMatchError::Projection)?;
        if let Some(prepared) = prepared {
            target
                .resource_mut::<ConfirmedProgressionLedger>()
                .commit_prepared(prepared);
            target.insert_resource(
                presentation.expect("a prepared result always has a typed presentation"),
            );
            self.confirmed_result = snapshot.confirmed_result;
        }
        Ok(Some(report))
    }

    #[cfg(test)]
    fn advance_manual(&self, ticks: u16) -> Result<(), EmbeddedOnlineMatchError> {
        self.commands
            .send(EmbeddedAuthorityCommand::AdvanceManual(ticks))
            .map_err(|_| EmbeddedOnlineMatchError::AuthorityThreadDisconnected)
    }

    #[cfg(test)]
    fn wait_until_published(&mut self, tick: SimTick) -> Result<(), EmbeddedOnlineMatchError> {
        self.inbox.wait_until_tick(tick, Duration::from_secs(5))
    }
}

impl Drop for ThreadedEmbeddedOnlineMatch {
    fn drop(&mut self) {
        self.force_shutdown.store(true, Ordering::Release);
        let _ = self.commands.try_send(EmbeddedAuthorityCommand::Stop);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn merge_pending_sample(pending: &mut Option<InputFrame>, sample: InputFrame) {
    match pending {
        Some(pending) => {
            pending.movement_x = sample.movement_x;
            pending.movement_y = sample.movement_y;
            pending.held_buttons = sample.held_buttons;
            pending.pressed_buttons = crate::network_protocol::InputButtons::new(
                pending.pressed_buttons.bits() | sample.pressed_buttons.bits(),
            )
            .expect("merging supported button masks stays supported");
            pending.released_buttons = crate::network_protocol::InputButtons::new(
                pending.released_buttons.bits() | sample.released_buttons.bits(),
            )
            .expect("merging supported button masks stays supported");
        }
        None => *pending = Some(sample),
    }
}

fn install_presentation_tick(
    source: &mut World,
    events: ThreadPresentationTick,
) -> Result<(), EmbeddedOnlineMatchError> {
    let mut buffer = TickEventBuffer::new(events.tick);
    for record in &events.events {
        let emitted = buffer
            .emit(record.event.id.source, record.event.kind)
            .map_err(LivePresentationProjectionError::from)
            .map_err(EmbeddedOnlineMatchError::Projection)?;
        if emitted != record.event.id {
            return Err(EmbeddedOnlineMatchError::Projection(
                LivePresentationProjectionError::EventIdentityChanged,
            ));
        }
    }
    source.resource_mut::<SimEventJournal>().commit(&buffer);
    for record in events.events {
        if let Some(intent) = record.combat {
            source
                .resource_mut::<CombatPresentationIntentJournal>()
                .record(intent)
                .map_err(projection_event_error)?;
        }
        if let Some(intent) = record.combat_cue {
            source
                .resource_mut::<CombatPresentationIntentJournal>()
                .record_cue(intent)
                .map_err(projection_event_error)?;
        }
        if let Some(intent) = record.fighter {
            source
                .resource_mut::<FighterPresentationIntentJournal>()
                .record(intent)
                .map_err(projection_event_error)?;
        }
        if let Some(intent) = record.item {
            source
                .resource_mut::<ItemPresentationIntentJournal>()
                .record(intent)
                .map_err(projection_event_error)?;
        }
        if let Some(intent) = record.arena {
            source
                .resource_mut::<ArenaPresentationIntentJournal>()
                .record(intent)
                .map_err(projection_event_error)?;
        }
        if let Some(intent) = record.special {
            source
                .resource_mut::<SpecialPresentationIntentJournal>()
                .record(intent)
                .map_err(projection_event_error)?;
        }
        if let Some(intent) = record.bee {
            source
                .resource_mut::<BeePresentationIntentJournal>()
                .record(intent)
                .map_err(projection_event_error)?;
        }
        if let Some(intent) = record.chick {
            source
                .resource_mut::<ChickPresentationIntentJournal>()
                .record(intent)
                .map_err(projection_event_error)?;
        }
        if let Some(intent) = record.penguin {
            source
                .resource_mut::<PenguinPresentationIntentJournal>()
                .record(intent)
                .map_err(projection_event_error)?;
        }
    }
    Ok(())
}

fn projection_event_error(error: EventEmitError) -> EmbeddedOnlineMatchError {
    EmbeddedOnlineMatchError::Projection(LivePresentationProjectionError::from(error))
}

#[allow(clippy::too_many_arguments)]
fn run_embedded_authority_worker(
    config: HeadlessMatchConfig,
    manifest: MatchManifest,
    peer_id: PeerId,
    input_config: AuthorityInputConfig,
    loopback_config: LocalLoopbackConfig,
    thread_config: AuthorityThreadConfig,
    clock: EmbeddedAuthorityClockMode,
    commands: Receiver<EmbeddedAuthorityCommand>,
    force_shutdown: &AtomicBool,
    publisher: ThreadProjectionPublisher,
) {
    let result = (|| -> Result<(), String> {
        let authority =
            build_headless_simulation(config.clone()).map_err(|error| error.to_string())?;
        let predicted = build_predicted_simulation(config).map_err(|error| error.to_string())?;
        let rollback = ThreadRollbackHooks::default();
        let predicted = PredictedClient::with_hooks(
            predicted,
            manifest.match_id,
            usize::from(manifest.snapshot_history_ticks),
            rollback.clone(),
            InstantRollbackTiming::default(),
        )
        .map_err(|error| error.to_string())?;
        let mut runner = LocalLoopbackMatch::with_client_world(
            manifest,
            peer_id,
            authority,
            predicted,
            input_config,
            loopback_config,
        )
        .map_err(|error| error.to_string())?;
        runner.start().map_err(|error| error.to_string())?;

        let mut last_published_tick = None;
        publish_worker_projection(
            &runner,
            &rollback,
            &publisher,
            None,
            &mut last_published_tick,
        )?;

        let mut inputs = [WorkerSeatInput::default(); MAX_SEATS];
        let mut schedule = SixtyHzSchedule::new();
        #[cfg(test)]
        let mut manual_ticks = 0_u32;
        loop {
            if force_shutdown.load(Ordering::Acquire) {
                return Ok(());
            }
            let should_tick = match clock {
                EmbeddedAuthorityClockMode::Realtime => wait_for_realtime_deadline(
                    &commands,
                    force_shutdown,
                    &mut inputs,
                    &schedule,
                    thread_config.max_commands_per_service,
                )?,
                #[cfg(test)]
                EmbeddedAuthorityClockMode::Manual => wait_for_manual_tick(
                    &commands,
                    force_shutdown,
                    &mut inputs,
                    &mut manual_ticks,
                    thread_config.max_commands_per_service,
                )?,
            };
            if !should_tick {
                return Ok(());
            }

            let report = advance_threaded_runner(&mut runner, peer_id, &mut inputs)?;
            publish_worker_projection(
                &runner,
                &rollback,
                &publisher,
                Some(report),
                &mut last_published_tick,
            )?;
            if report.final_result_id.is_some() {
                return Ok(());
            }
            if matches!(clock, EmbeddedAuthorityClockMode::Realtime) {
                schedule.advance();
            }
        }
    })();
    publisher.finish(result);
}

fn wait_for_realtime_deadline(
    commands: &Receiver<EmbeddedAuthorityCommand>,
    force_shutdown: &AtomicBool,
    inputs: &mut [WorkerSeatInput; MAX_SEATS],
    schedule: &SixtyHzSchedule,
    max_commands: usize,
) -> Result<bool, String> {
    loop {
        if force_shutdown.load(Ordering::Acquire) {
            return Ok(false);
        }
        let deadline = schedule.deadline();
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        match commands.recv_timeout(deadline.duration_since(now)) {
            Ok(command) => {
                if !service_embedded_command(command, inputs, None)? {
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
                if !service_embedded_command(command, inputs, None)? {
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
fn wait_for_manual_tick(
    commands: &Receiver<EmbeddedAuthorityCommand>,
    force_shutdown: &AtomicBool,
    inputs: &mut [WorkerSeatInput; MAX_SEATS],
    manual_ticks: &mut u32,
    max_commands: usize,
) -> Result<bool, String> {
    while *manual_ticks == 0 {
        if force_shutdown.load(Ordering::Acquire) {
            return Ok(false);
        }
        let command = commands
            .recv()
            .map_err(|_| "manual command channel disconnected".to_owned())?;
        if !service_embedded_command(command, inputs, Some(manual_ticks))? {
            return Ok(false);
        }
    }
    for _ in 0..max_commands.saturating_sub(1) {
        match commands.try_recv() {
            Ok(command) => {
                if !service_embedded_command(command, inputs, Some(manual_ticks))? {
                    return Ok(false);
                }
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => return Ok(false),
        }
    }
    *manual_ticks -= 1;
    Ok(true)
}

fn service_embedded_command(
    command: EmbeddedAuthorityCommand,
    inputs: &mut [WorkerSeatInput; MAX_SEATS],
    #[cfg_attr(not(test), allow(unused_variables))] manual_ticks: Option<&mut u32>,
) -> Result<bool, String> {
    match command {
        EmbeddedAuthorityCommand::InputSamples(samples) => {
            for (seat, sample) in samples.into_iter().enumerate() {
                if let Some(sample) = sample {
                    sample.validate().map_err(|error| error.to_string())?;
                    if usize::from(sample.seat.get()) != seat {
                        return Err("input sample occupied the wrong fixed seat slot".to_owned());
                    }
                    inputs[seat].merge(sample);
                }
            }
            Ok(true)
        }
        #[cfg(test)]
        EmbeddedAuthorityCommand::AdvanceManual(ticks) => {
            if ticks == 0 {
                return Err("manual authority advance must be non-zero".to_owned());
            }
            let manual_ticks = manual_ticks
                .ok_or_else(|| "manual advance reached the realtime authority clock".to_owned())?;
            *manual_ticks = manual_ticks.saturating_add(u32::from(ticks));
            Ok(true)
        }
        EmbeddedAuthorityCommand::Stop => Ok(false),
    }
}

fn advance_threaded_runner(
    runner: &mut ThreadedLiveLoopback,
    peer_id: PeerId,
    inputs: &mut [WorkerSeatInput; MAX_SEATS],
) -> Result<AuthorityTickReport, String> {
    let tick = runner
        .client_world()
        .predicted_tick()
        .ok_or_else(|| "predicted worker timeline is not initialized".to_owned())?
        .next();
    let mut prediction = [None; MAX_SEATS];
    let mut local_frames = Vec::with_capacity(MAX_SEATS);
    for assignment in runner.manifest().ownership.as_slice() {
        if assignment.owner != SeatOwner::Peer(peer_id) {
            continue;
        }
        let seat = assignment.seat;
        let frame = inputs[usize::from(seat.get())].frame_for_tick(tick, seat);
        prediction[usize::from(seat.get())] = Some(frame);
        local_frames.push(frame);
    }
    if local_frames.is_empty() {
        return Err("embedded authority has no locally owned human seat".to_owned());
    }
    runner
        .client_world_mut()
        .predict_next(prediction)
        .map_err(|error| error.to_string())?;
    runner
        .run_local_tick(&local_frames)
        .map_err(|error| error.to_string())
}

fn publish_worker_projection(
    runner: &ThreadedLiveLoopback,
    rollback: &ThreadRollbackHooks,
    publisher: &ThreadProjectionPublisher,
    authority: Option<AuthorityTickReport>,
    last_published_tick: &mut Option<SimTick>,
) -> Result<(), String> {
    let client = runner.client_world();
    let driver = client.world();
    let source = driver.world();
    let journal = source
        .get_resource::<SimEventJournal>()
        .ok_or_else(|| "predicted worker is missing SimEventJournal".to_owned())?;
    let rollback_retain_through = rollback.take();
    let current_tick = client
        .predicted_tick()
        .ok_or_else(|| "predicted worker lost its initialized timeline".to_owned())?;
    let start = rollback_retain_through
        .and_then(|tick| tick.0.checked_add(1).map(SimTick))
        .or_else(|| last_published_tick.and_then(|tick| tick.0.checked_add(1).map(SimTick)))
        .or_else(|| journal.oldest_tick());
    let mut event_ticks = Vec::new();
    if let Some(mut tick) = start {
        while tick <= current_tick {
            if let Some(events) = ThreadPresentationTick::capture(source, tick)? {
                event_ticks.push(events);
            }
            if tick == current_tick {
                break;
            }
            tick = tick.next();
        }
    }
    let canonical = driver
        .capture_live_snapshot()
        .map_err(|error| error.to_string())?;
    publisher.publish(
        ThreadProjectionSnapshot {
            snapshot: canonical,
            confirmed_through: client.confirmed_tick(),
            authority,
            confirmed_result: runner.confirmed_result(),
        },
        rollback_retain_through,
        event_ticks,
    );
    *last_published_tick = Some(current_tick);
    Ok(())
}

/// Reconciles menu/match lifecycle before Bevy decides whether to run a fixed
/// step. The local canonical set chain is switched off before the first online
/// tick can execute.
pub fn reconcile_embedded_online_client(world: &mut World) {
    let (requested, request_revision) = world
        .get_resource::<user_mode::UserModeState>()
        .map(|state| {
            (
                state.network_match_requested(),
                state.match_request_revision(),
            )
        })
        .unwrap_or((false, 0));

    let Some(mut controller) = world.remove_non_send_resource::<EmbeddedOnlineClientController>()
    else {
        return;
    };

    if !requested {
        if controller.owns_projection_target {
            controller.active = None;
            controller.owns_projection_target = false;
            controller.failed_seed = None;
            controller.started_revision = None;
            controller.failed_revision = None;
            crate::presentation_projection::release_projection_target(world);
            if let Some(mut status) = world.get_resource_mut::<EmbeddedOnlineClientStatus>() {
                *status = EmbeddedOnlineClientStatus::default();
            }
        }
        world.insert_non_send_resource(controller);
        return;
    }

    let new_request = controller.started_revision != Some(request_revision)
        && controller.failed_revision != Some(request_revision);
    if new_request {
        controller.active = None;
        controller.failed_seed = None;
        controller.failed_revision = None;
        if controller.owns_projection_target {
            controller.owns_projection_target = false;
            crate::presentation_projection::release_projection_target(world);
        }
    }

    let current_seed = world
        .get_resource::<game_state::LocalSetup>()
        .map(|setup| setup.replay_seed);
    if controller.active.is_none()
        && new_request
        && current_seed.is_none_or(|seed| controller.failed_seed != Some(seed))
    {
        if let Some(mut status) = world.get_resource_mut::<EmbeddedOnlineClientStatus>() {
            status.phase = EmbeddedOnlineClientPhase::Starting;
            status.failure = None;
        }
        controller.owns_projection_target = true;
        world.insert_resource(simulation::SimulationDriveMode::ExternalProjection);

        let result = start_embedded_online_client(&mut controller, world);
        match result {
            Ok(online) => {
                controller.active = Some(online);
                controller.failed_seed = None;
                controller.started_revision = Some(request_revision);
                controller.failed_revision = None;
                if let Some(mut status) = world.get_resource_mut::<EmbeddedOnlineClientStatus>() {
                    status.phase = EmbeddedOnlineClientPhase::Fighting;
                    status.latest_tick = Some(SimTick::ZERO);
                }
            }
            Err(error) => {
                fail_embedded_online_client(
                    &mut controller,
                    world,
                    request_revision,
                    current_seed,
                    embedded_startup_failure(&error),
                );
            }
        }
    }

    world.insert_non_send_resource(controller);
}

fn start_embedded_online_client(
    controller: &mut EmbeddedOnlineClientController,
    target: &mut World,
) -> Result<ThreadedEmbeddedOnlineMatch, EmbeddedOnlineMatchError> {
    #[cfg(test)]
    if std::mem::take(&mut controller.injected_startup_failure) {
        return Err(EmbeddedOnlineMatchError::AuthorityThreadBootstrap(
            "deterministic injected startup failure".to_owned(),
        ));
    }

    let setup = target
        .get_resource::<game_state::LocalSetup>()
        .cloned()
        .ok_or(EmbeddedOnlineMatchError::TimelineExhausted)?;
    controller.session_counter = controller.session_counter.wrapping_add(1);
    let peer_id = PeerId::new(1).expect("the embedded local peer ID is non-zero");
    let match_id = embedded_match_id(setup.replay_seed, controller.session_counter);
    let config = build_headless_match_config(
        &setup,
        MatchBuildOptions::single_peer(
            match_id,
            AuthorityKind::Offline,
            false,
            peer_id,
            &setup,
            SimTick(2),
        ),
    )
    .map_err(EmbeddedOnlineMatchError::MatchConfig)?;
    let mut online = ThreadedEmbeddedOnlineMatch::new(
        config,
        peer_id,
        AuthorityInputConfig::default(),
        LocalLoopbackConfig::default(),
        AuthorityThreadConfig::default(),
    )?;
    online.prepare_presentation_target(target)?;
    Ok(online)
}

fn embedded_match_id(seed: u64, session_counter: u64) -> MatchId {
    let first = seed ^ 0x4146_432d_4c4f_4341;
    let second = session_counter.rotate_left(23) ^ 0x4c2d_4d41_5443_4821;
    let mut bytes = [0_u8; 16];
    bytes[..8].copy_from_slice(&first.to_le_bytes());
    bytes[8..].copy_from_slice(&second.to_le_bytes());
    MatchId::new(bytes).expect("the embedded match namespace is non-zero")
}

/// Executes one online tick outside the disabled canonical set chain.
pub fn drive_embedded_online_client(world: &mut World) {
    if world
        .get_resource::<simulation::SimulationDriveMode>()
        .is_none_or(|mode| *mode != simulation::SimulationDriveMode::ExternalProjection)
    {
        return;
    }
    let Some(mut controller) = world.remove_non_send_resource::<EmbeddedOnlineClientController>()
    else {
        return;
    };

    if controller
        .active
        .as_ref()
        .is_some_and(|online| online.confirmed_result().is_some())
    {
        if let Some(mut status) = world.get_resource_mut::<EmbeddedOnlineClientStatus>() {
            status.phase = EmbeddedOnlineClientPhase::Results;
        }
        world.insert_non_send_resource(controller);
        return;
    }

    #[cfg(test)]
    let injected_runtime_failure = std::mem::take(&mut controller.injected_runtime_failure);
    #[cfg(not(test))]
    let injected_runtime_failure = false;
    let outcome = if injected_runtime_failure {
        Err(EmbeddedOnlineMatchError::AuthorityThreadFailed(
            "deterministic injected runtime failure".to_owned(),
        ))
    } else if let Some(online) = controller.active.as_mut() {
        world.resource_scope::<LocalTickInputState, _>(|world, mut inputs| {
            online.service(&mut inputs, world)
        })
    } else {
        world.insert_non_send_resource(controller);
        return;
    };

    match outcome {
        Ok(Some(report)) => {
            if let Some(mut status) = world.get_resource_mut::<EmbeddedOnlineClientStatus>() {
                status.latest_tick = Some(report.authority.tick);
                status.phase = if report.authority.final_result_id.is_some() {
                    EmbeddedOnlineClientPhase::Results
                } else {
                    EmbeddedOnlineClientPhase::Fighting
                };
                status.failure = None;
            }
        }
        Ok(None) => {}
        Err(error) => {
            let failed_revision = controller.started_revision.unwrap_or_default();
            let failed_seed = world
                .get_resource::<game_state::LocalSetup>()
                .map(|setup| setup.replay_seed);
            fail_embedded_online_client(
                &mut controller,
                world,
                failed_revision,
                failed_seed,
                embedded_runtime_failure(&error),
            );
        }
    }
    world.insert_non_send_resource(controller);
}

const EMBEDDED_STARTUP_FAILURE_DETAIL: u16 = 200;
const EMBEDDED_RUNTIME_FAILURE_DETAIL: u16 = 201;
const EMBEDDED_SYNCHRONIZATION_FAILURE_DETAIL: u16 = 202;

fn embedded_startup_failure(_error: &EmbeddedOnlineMatchError) -> OnlineFailure {
    OnlineFailure {
        code: OnlineFailureCode::InternalFailure,
        severity: OnlineFailureSeverity::Recoverable,
        recovery: OnlineRecoveryAction::Retry,
        detail_code: EMBEDDED_STARTUP_FAILURE_DETAIL,
    }
}

fn embedded_runtime_failure(error: &EmbeddedOnlineMatchError) -> OnlineFailure {
    let (code, detail_code) = match error {
        EmbeddedOnlineMatchError::AuthorityThreadBootstrap(_)
        | EmbeddedOnlineMatchError::AuthorityThreadDisconnected
        | EmbeddedOnlineMatchError::AuthorityThreadFailed(_) => (
            OnlineFailureCode::AuthorityLost,
            EMBEDDED_RUNTIME_FAILURE_DETAIL,
        ),
        _ => (
            OnlineFailureCode::SynchronizationFailed,
            EMBEDDED_SYNCHRONIZATION_FAILURE_DETAIL,
        ),
    };
    OnlineFailure {
        code,
        severity: OnlineFailureSeverity::MatchEnded,
        recovery: OnlineRecoveryAction::Retry,
        detail_code,
    }
}

fn fail_embedded_online_client(
    controller: &mut EmbeddedOnlineClientController,
    world: &mut World,
    request_revision: u64,
    replay_seed: Option<u64>,
    failure: OnlineFailure,
) {
    debug_assert!(
        controller.owns_projection_target,
        "only an owned embedded projection may enter the failed lifecycle"
    );
    controller.active = None;
    controller.failed_revision = Some(request_revision);
    controller.failed_seed = replay_seed;

    // `release_projection_target` ordinarily hands the rendered world back to
    // direct local simulation. That is correct after an explicit menu exit,
    // but fatal while this failed request is still active: the old rendered
    // world would become a second, unauthoritative continuation. Clear all
    // match-scoped projection state, then keep the complete canonical schedule
    // fenced until the player cancels or creates a new request revision.
    crate::presentation_projection::release_projection_target(world);
    world.insert_resource(simulation::SimulationDriveMode::ExternalProjection);

    if let Some(mut status) = world.get_resource_mut::<EmbeddedOnlineClientStatus>() {
        status.phase = EmbeddedOnlineClientPhase::Failed;
        status.failure = Some(failure);
    }
    if let Some(mut user_mode) = world.get_resource_mut::<user_mode::UserModeState>() {
        user_mode.present_embedded_authority_failure();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::hint::black_box;

    use crate::components::PlayerKeyBindings;
    use crate::determinism::{SimEntityId, SimEntityKind};
    use crate::ecs_identity::StableSimEntity;
    use crate::game_state::LocalSetup;
    use crate::items::{ArenaItem, ItemKind};
    use crate::match_config::{MatchBuildOptions, build_headless_match_config};
    use crate::network_protocol::{AuthorityKind, MatchId};
    use crate::user_mode::{UserModeResultPanel, UserModeResultText, UserModeState};

    fn config(peer: PeerId) -> HeadlessMatchConfig {
        let setup = LocalSetup::default();
        build_headless_match_config(
            &setup,
            MatchBuildOptions::single_peer(
                MatchId::new(*b"embedded-online1").unwrap(),
                AuthorityKind::Offline,
                false,
                peer,
                &setup,
                SimTick(2),
            ),
        )
        .unwrap()
    }

    fn controller_target() -> LiveSimulationDriver {
        let peer = PeerId::new(31).unwrap();
        let mut target =
            build_headless_simulation(config(peer)).expect("controller projection target builds");
        target
            .world_mut()
            .insert_resource(simulation::SimulationDriveMode::Local);
        target
            .world_mut()
            .insert_resource(EmbeddedOnlineClientStatus::default());
        target
            .world_mut()
            .insert_resource(LocalTickInputState::default());
        target
            .world_mut()
            .insert_resource(PlayerKeyBindings::default());
        let mut user_mode = UserModeState::default();
        user_mode.request_embedded_match_for_test();
        target.world_mut().insert_resource(user_mode);
        target
    }

    fn idle_controller_target() -> LiveSimulationDriver {
        let peer = PeerId::new(32).unwrap();
        let mut target =
            build_headless_simulation(config(peer)).expect("idle projection target builds");
        target
            .world_mut()
            .insert_resource(simulation::SimulationDriveMode::Local);
        target
            .world_mut()
            .insert_resource(EmbeddedOnlineClientStatus::default());
        target.world_mut().insert_resource(UserModeState::default());
        target
            .world_mut()
            .insert_non_send_resource(EmbeddedOnlineClientController::default());
        target
    }

    fn spawn_test_arena_item(world: &mut World, index: u32) -> Entity {
        let position = Vec3::new(3.0 + index as f32, 2.0, -1.0);
        let mut item = ArenaItem::new(ItemKind::Apple, position, 0.25);
        item.durability = 7;
        world
            .spawn((
                StableSimEntity::new(SimEntityId::new(SimEntityKind::Item, index, 1)),
                item,
            ))
            .id()
    }

    fn assert_local_simulation_is_fenced(world: &mut World) {
        let tick_before = *world.resource::<SimTick>();
        let mut schedule = Schedule::default();
        schedule.add_systems(
            simulation::advance_sim_tick.run_if(simulation::local_simulation_drive_enabled),
        );
        schedule.run(world);
        assert_eq!(*world.resource::<SimTick>(), tick_before);
        assert_eq!(
            *world.resource::<simulation::SimulationDriveMode>(),
            simulation::SimulationDriveMode::ExternalProjection
        );
    }

    fn assert_failure_is_visible_in_user_mode(world: &mut World, expected: &str) {
        let panel = world
            .spawn((
                UserModeResultPanel,
                Node {
                    display: Display::None,
                    ..default()
                },
            ))
            .id();
        let text = world
            .spawn((UserModeResultText, Text::new("not projected")))
            .id();
        let mut schedule = Schedule::default();
        schedule.add_systems(user_mode::update_user_mode_ui);
        schedule.run(world);
        assert_eq!(world.get::<Node>(panel).unwrap().display, Display::Flex);
        assert!(
            world.get::<Text>(text).unwrap().as_str().contains(expected),
            "failure text was {:?}",
            world.get::<Text>(text).unwrap().as_str()
        );
    }

    #[test]
    fn repeated_idle_reconcile_preserves_offline_items_and_local_drive() {
        let mut target = idle_controller_target();
        let item_entity = spawn_test_arena_item(target.world_mut(), 15);
        let expected_position = target
            .world()
            .get::<ArenaItem>(item_entity)
            .expect("offline arena item exists")
            .position;

        for _ in 0..4 {
            reconcile_embedded_online_client(target.world_mut());

            let item = target
                .world()
                .get::<ArenaItem>(item_entity)
                .expect("idle reconciliation preserves offline arena items");
            assert_eq!(item.position, expected_position);
            assert_eq!(item.durability, 7);
            assert_eq!(
                *target.world().resource::<simulation::SimulationDriveMode>(),
                simulation::SimulationDriveMode::Local
            );
            assert_eq!(
                *target.world().resource::<EmbeddedOnlineClientStatus>(),
                EmbeddedOnlineClientStatus::default()
            );
            assert!(
                !target
                    .world()
                    .non_send_resource::<EmbeddedOnlineClientController>()
                    .owns_projection_target
            );
        }

        target
            .world_mut()
            .insert_resource(simulation::SimulationDriveMode::ExternalProjection);
        reconcile_embedded_online_client(target.world_mut());
        assert!(
            target.world().get::<ArenaItem>(item_entity).is_some(),
            "an unowned external projection belongs to another client lifecycle"
        );
        assert_eq!(
            *target.world().resource::<simulation::SimulationDriveMode>(),
            simulation::SimulationDriveMode::ExternalProjection
        );
    }

    #[test]
    fn leaving_owned_projection_cleans_once_then_idle_preserves_local_state() {
        let mut target = idle_controller_target();
        let online_item = spawn_test_arena_item(target.world_mut(), 15);
        target
            .world_mut()
            .insert_resource(simulation::SimulationDriveMode::ExternalProjection);
        *target
            .world_mut()
            .resource_mut::<EmbeddedOnlineClientStatus>() = EmbeddedOnlineClientStatus {
            phase: EmbeddedOnlineClientPhase::Fighting,
            latest_tick: Some(SimTick(11)),
            failure: None,
        };
        target
            .world_mut()
            .insert_non_send_resource(EmbeddedOnlineClientController {
                owns_projection_target: true,
                started_revision: Some(4),
                ..default()
            });

        reconcile_embedded_online_client(target.world_mut());

        assert!(
            target.world().get_entity(online_item).is_err(),
            "exiting an owned online projection clears its dynamic roots"
        );
        assert_eq!(
            *target.world().resource::<simulation::SimulationDriveMode>(),
            simulation::SimulationDriveMode::Local
        );
        assert_eq!(
            *target.world().resource::<EmbeddedOnlineClientStatus>(),
            EmbeddedOnlineClientStatus::default()
        );
        {
            let controller = target
                .world()
                .non_send_resource::<EmbeddedOnlineClientController>();
            assert!(!controller.owns_projection_target);
            assert_eq!(controller.started_revision, None);
        }

        let new_offline_item = spawn_test_arena_item(target.world_mut(), 14);
        reconcile_embedded_online_client(target.world_mut());
        reconcile_embedded_online_client(target.world_mut());

        assert!(
            target.world().get::<ArenaItem>(new_offline_item).is_some(),
            "idempotent idle reconciliation cannot repeat online cleanup"
        );
    }

    /// Exact replica of the legacy `requested == false` branch retained only
    /// for the same-hardware compliance capture below. The fixture is first
    /// normalized to the state this branch produces, so legacy and current
    /// paths have identical observable outcomes during timing.
    fn legacy_idle_reconcile_for_hot_path_capture(world: &mut World) {
        let requested = world
            .get_resource::<user_mode::UserModeState>()
            .is_some_and(user_mode::UserModeState::network_match_requested);
        let Some(mut controller) =
            world.remove_non_send_resource::<EmbeddedOnlineClientController>()
        else {
            return;
        };

        if !requested {
            controller.active = None;
            controller.failed_seed = None;
            controller.started_revision = None;
            controller.failed_revision = None;
            crate::presentation_projection::release_projection_target(world);
            if let Some(mut status) = world.get_resource_mut::<EmbeddedOnlineClientStatus>() {
                *status = EmbeddedOnlineClientStatus::default();
            }
        }

        world.insert_non_send_resource(controller);
    }

    fn normalized_idle_reconcile_capture_target()
    -> (LiveSimulationDriver, crate::snapshot_ecs::SnapshotContract) {
        let mut target = idle_controller_target();
        let snapshot_contract = *target
            .world()
            .resource::<crate::snapshot_ecs::SnapshotContract>();
        legacy_idle_reconcile_for_hot_path_capture(target.world_mut());
        let dynamic_roots = {
            let world = target.world_mut();
            let mut query = world.query::<&StableSimEntity>();
            query.iter(world).count()
        };
        assert_eq!(
            dynamic_roots, 0,
            "the state-equivalent capture cannot contain match-owned dynamic roots"
        );
        (target, snapshot_contract)
    }

    fn assert_idle_reconcile_capture_equivalent(
        legacy: &mut LiveSimulationDriver,
        legacy_contract: crate::snapshot_ecs::SnapshotContract,
        current: &mut LiveSimulationDriver,
        current_contract: crate::snapshot_ecs::SnapshotContract,
    ) {
        assert!(
            !legacy
                .world()
                .contains_resource::<crate::snapshot_ecs::SnapshotContract>()
        );
        assert!(
            !current
                .world()
                .contains_resource::<crate::snapshot_ecs::SnapshotContract>()
        );
        legacy.world_mut().insert_resource(legacy_contract);
        current.world_mut().insert_resource(current_contract);
        let legacy_hash = legacy.state_hash().unwrap();
        let current_hash = current.state_hash().unwrap();
        legacy
            .world_mut()
            .remove_resource::<crate::snapshot_ecs::SnapshotContract>();
        current
            .world_mut()
            .remove_resource::<crate::snapshot_ecs::SnapshotContract>();
        assert_eq!(legacy_hash, current_hash);
        assert_eq!(
            legacy.world().entities().len(),
            current.world().entities().len()
        );
        assert_eq!(
            legacy.world().resource::<simulation::SimulationDriveMode>(),
            current
                .world()
                .resource::<simulation::SimulationDriveMode>()
        );
        assert_eq!(
            legacy.world().resource::<EmbeddedOnlineClientStatus>(),
            current.world().resource::<EmbeddedOnlineClientStatus>()
        );
        for target in [&*legacy, &*current] {
            let controller = target
                .world()
                .non_send_resource::<EmbeddedOnlineClientController>();
            assert!(controller.active.is_none());
            assert!(!controller.owns_projection_target);
            assert_eq!(controller.failed_seed, None);
            assert_eq!(controller.started_revision, None);
            assert_eq!(controller.failed_revision, None);
        }
    }

    #[derive(Clone, Copy, Debug)]
    struct IdleReconcileTiming {
        p50_ns: u64,
        p95_ns: u64,
        p99_ns: u64,
    }

    fn percentile_ns(samples: &mut [u64], percentile: usize) -> u64 {
        samples.sort_unstable();
        let rank = (samples.len() * percentile).div_ceil(100);
        samples[rank.saturating_sub(1).min(samples.len() - 1)]
    }

    fn summarize_idle_reconcile_samples(mut samples: Vec<u64>) -> IdleReconcileTiming {
        let mut p50_samples = samples.clone();
        let mut p95_samples = samples.clone();
        IdleReconcileTiming {
            p50_ns: percentile_ns(&mut p50_samples, 50),
            p95_ns: percentile_ns(&mut p95_samples, 95),
            p99_ns: percentile_ns(&mut samples, 99),
        }
    }

    fn capture_idle_reconcile_samples(
        target: &mut LiveSimulationDriver,
        reconcile: fn(&mut World),
        sample_count: usize,
        calls_per_sample: usize,
    ) -> Vec<u64> {
        (0..sample_count)
            .map(|_| {
                let started = Instant::now();
                for _ in 0..calls_per_sample {
                    reconcile(black_box(target.world_mut()));
                }
                let elapsed = started.elapsed().as_nanos();
                u64::try_from(elapsed / calls_per_sample as u128).unwrap()
            })
            .collect()
    }

    /// Same-hardware rule-4 diagnostic for the per-frame idle coordinator.
    ///
    /// Run explicitly with the profiling profile; ordinary test runs skip the
    /// capture because wall-clock numbers are evidence, not a correctness gate.
    #[test]
    #[ignore = "same-hardware performance compliance capture"]
    fn idle_reconcile_hot_path_capture() {
        const PAIRS: usize = 9;
        const WARMUP_CALLS: usize = 256;
        const SAMPLE_COUNT: usize = 128;
        const CALLS_PER_SAMPLE: usize = 10;

        let mut legacy_p50 = Vec::with_capacity(PAIRS);
        let mut legacy_p95 = Vec::with_capacity(PAIRS);
        let mut legacy_p99 = Vec::with_capacity(PAIRS);
        let mut current_p50 = Vec::with_capacity(PAIRS);
        let mut current_p95 = Vec::with_capacity(PAIRS);
        let mut current_p99 = Vec::with_capacity(PAIRS);

        for pair in 0..PAIRS {
            let (mut legacy, legacy_contract) = normalized_idle_reconcile_capture_target();
            let (mut current, current_contract) = normalized_idle_reconcile_capture_target();
            assert_idle_reconcile_capture_equivalent(
                &mut legacy,
                legacy_contract,
                &mut current,
                current_contract,
            );

            for _ in 0..WARMUP_CALLS {
                legacy_idle_reconcile_for_hot_path_capture(black_box(legacy.world_mut()));
                reconcile_embedded_online_client(black_box(current.world_mut()));
            }
            assert_idle_reconcile_capture_equivalent(
                &mut legacy,
                legacy_contract,
                &mut current,
                current_contract,
            );

            let (legacy_samples, current_samples) = if pair % 2 == 0 {
                let legacy_samples = capture_idle_reconcile_samples(
                    &mut legacy,
                    legacy_idle_reconcile_for_hot_path_capture,
                    SAMPLE_COUNT,
                    CALLS_PER_SAMPLE,
                );
                let current_samples = capture_idle_reconcile_samples(
                    &mut current,
                    reconcile_embedded_online_client,
                    SAMPLE_COUNT,
                    CALLS_PER_SAMPLE,
                );
                (legacy_samples, current_samples)
            } else {
                let current_samples = capture_idle_reconcile_samples(
                    &mut current,
                    reconcile_embedded_online_client,
                    SAMPLE_COUNT,
                    CALLS_PER_SAMPLE,
                );
                let legacy_samples = capture_idle_reconcile_samples(
                    &mut legacy,
                    legacy_idle_reconcile_for_hot_path_capture,
                    SAMPLE_COUNT,
                    CALLS_PER_SAMPLE,
                );
                (legacy_samples, current_samples)
            };
            let legacy_timing = summarize_idle_reconcile_samples(legacy_samples);
            let current_timing = summarize_idle_reconcile_samples(current_samples);
            legacy_p50.push(legacy_timing.p50_ns);
            legacy_p95.push(legacy_timing.p95_ns);
            legacy_p99.push(legacy_timing.p99_ns);
            current_p50.push(current_timing.p50_ns);
            current_p95.push(current_timing.p95_ns);
            current_p99.push(current_timing.p99_ns);
            assert_idle_reconcile_capture_equivalent(
                &mut legacy,
                legacy_contract,
                &mut current,
                current_contract,
            );

            println!(
                "AFC_IDLE_RECONCILE_PERF_PAIR {{\"pair\":{},\"legacy_p50_ns\":{},\"legacy_p95_ns\":{},\"legacy_p99_ns\":{},\"current_p50_ns\":{},\"current_p95_ns\":{},\"current_p99_ns\":{},\"samples\":{},\"calls_per_sample\":{}}}",
                pair + 1,
                legacy_timing.p50_ns,
                legacy_timing.p95_ns,
                legacy_timing.p99_ns,
                current_timing.p50_ns,
                current_timing.p95_ns,
                current_timing.p99_ns,
                SAMPLE_COUNT,
                CALLS_PER_SAMPLE,
            );
        }

        let legacy = IdleReconcileTiming {
            p50_ns: percentile_ns(&mut legacy_p50, 50),
            p95_ns: percentile_ns(&mut legacy_p95, 50),
            p99_ns: percentile_ns(&mut legacy_p99, 50),
        };
        let current = IdleReconcileTiming {
            p50_ns: percentile_ns(&mut current_p50, 50),
            p95_ns: percentile_ns(&mut current_p95, 50),
            p99_ns: percentile_ns(&mut current_p99, 50),
        };
        println!(
            "AFC_IDLE_RECONCILE_PERF_RESULT {{\"schema_version\":1,\"pairs\":{},\"samples_per_pair\":{},\"calls_per_sample\":{},\"legacy_median_p50_ns\":{},\"legacy_median_p95_ns\":{},\"legacy_median_p99_ns\":{},\"current_median_p50_ns\":{},\"current_median_p95_ns\":{},\"current_median_p99_ns\":{}}}",
            PAIRS,
            SAMPLE_COUNT,
            CALLS_PER_SAMPLE,
            legacy.p50_ns,
            legacy.p95_ns,
            legacy.p99_ns,
            current.p50_ns,
            current.p95_ns,
            current.p99_ns,
        );
    }

    #[test]
    fn embedded_match_uses_prediction_authority_and_projection_worlds() {
        let peer = PeerId::new(7).unwrap();
        let config = config(peer);
        let mut presentation_target =
            build_headless_simulation(config.clone()).expect("projection fixture builds");
        let mut online = EmbeddedOnlineMatch::new(
            config,
            peer,
            AuthorityInputConfig::default(),
            LocalLoopbackConfig::default(),
        )
        .expect("serialized loopback starts");

        let initial = online
            .prepare_presentation_target(presentation_target.world_mut())
            .expect("initial predicted snapshot projects");
        assert_eq!(initial.snapshot_tick, SimTick::ZERO);
        assert_eq!(
            presentation_target.state_hash().unwrap(),
            online.runner().client_world().world().state_hash().unwrap()
        );

        let mut inputs = LocalTickInputState::default();
        for expected in 1..=4 {
            let report = online
                .tick(&mut inputs, presentation_target.world_mut())
                .expect("online tick completes");
            assert_eq!(report.authority.tick, SimTick(expected));
            assert_eq!(report.presentation.snapshot_tick, SimTick(expected));
            assert_eq!(
                presentation_target.state_hash().unwrap(),
                online.runner().client_world().world().state_hash().unwrap()
            );
        }
    }

    #[test]
    fn render_stall_does_not_stop_threaded_authority_and_projection_catches_up() {
        let peer = PeerId::new(9).unwrap();
        let config = config(peer);
        let mut presentation_target =
            build_headless_simulation(config.clone()).expect("projection fixture builds");
        let mut online = ThreadedEmbeddedOnlineMatch::new_with_clock(
            config,
            peer,
            AuthorityInputConfig::default(),
            LocalLoopbackConfig::default(),
            AuthorityThreadConfig::default(),
            EmbeddedAuthorityClockMode::Manual,
        )
        .expect("manual-clock serialized worker starts");

        let initial = online
            .prepare_presentation_target(presentation_target.world_mut())
            .expect("initial synchronized snapshot projects");
        assert_eq!(initial.snapshot_tick, SimTick::ZERO);
        assert_eq!(presentation_target.current_sim_tick(), SimTick::ZERO);

        // No render/client service call occurs while the worker receives eight
        // independent 60 Hz deadlines. The manual deadline source avoids a
        // sleep-based timing assertion while exercising the real worker loop.
        online.advance_manual(8).expect("deadlines are queued");
        online
            .wait_until_published(SimTick(8))
            .expect("authority reaches the requested deadline");
        assert_eq!(presentation_target.current_sim_tick(), SimTick::ZERO);

        let mut inputs = LocalTickInputState::default();
        let report = online
            .service(&mut inputs, presentation_target.world_mut())
            .expect("stalled render target reconciles")
            .expect("the worker published a newer snapshot");
        let projection = report.presentation;
        let authority = report.authority;
        assert_eq!(authority.tick, SimTick(8));
        assert_eq!(online.sample_tick, 9);
        assert_eq!(projection.snapshot_tick, SimTick(8));
        assert_eq!(projection.confirmed_through, Some(SimTick(8)));
        assert_eq!(presentation_target.current_sim_tick(), SimTick(8));
        assert_eq!(
            presentation_target.state_hash().unwrap(),
            authority.state_hash.0
        );
    }

    #[test]
    fn confirmed_result_slot_is_persistent_while_cosmetic_overflow_is_observable() {
        let mut state = ThreadProjectionMailboxState::default();
        let result = ConfirmedSessionResult {
            result_id: 44,
            final_tick: SimTick(9),
            final_hash: crate::network_protocol::StateHash(81),
        };
        state.observe_confirmed_result(result);
        state.observe_confirmed_result(result);
        for tick in 0..=SIM_EVENT_HISTORY_TICKS as u64 {
            state.push_event_tick(ThreadPresentationTick {
                tick: SimTick(tick),
                events: Vec::new(),
            });
        }

        assert_eq!(state.event_ticks.len(), SIM_EVENT_HISTORY_TICKS);
        assert_eq!(state.dropped_event_ticks, 1);
        assert_eq!(state.confirmed_result, Some(result));

        let shared = Arc::new(Mutex::new(state));
        let (_signal_tx, signal_rx) = mpsc::sync_channel(1);
        let mut inbox = ThreadProjectionInbox {
            state: Arc::clone(&shared),
            signal: signal_rx,
        };
        assert_eq!(inbox.drain().confirmed_result, Some(result));
        assert_eq!(inbox.drain().confirmed_result, Some(result));
    }

    #[test]
    fn startup_failure_stays_fenced_and_requires_a_new_request_revision() {
        let mut target = controller_target();
        target
            .world_mut()
            .insert_non_send_resource(EmbeddedOnlineClientController {
                injected_startup_failure: true,
                ..default()
            });

        reconcile_embedded_online_client(target.world_mut());

        let status = target.world().resource::<EmbeddedOnlineClientStatus>();
        assert_eq!(status.phase, EmbeddedOnlineClientPhase::Failed);
        assert_eq!(
            status.failure,
            Some(OnlineFailure {
                code: OnlineFailureCode::InternalFailure,
                severity: OnlineFailureSeverity::Recoverable,
                recovery: OnlineRecoveryAction::Retry,
                detail_code: EMBEDDED_STARTUP_FAILURE_DETAIL,
            })
        );
        assert!(
            target
                .world()
                .resource::<UserModeState>()
                .network_match_requested()
        );
        assert_local_simulation_is_fenced(target.world_mut());
        assert_failure_is_visible_in_user_mode(target.world_mut(), "COULD NOT START");

        for _ in 0..3 {
            reconcile_embedded_online_client(target.world_mut());
            let controller = target
                .world()
                .non_send_resource::<EmbeddedOnlineClientController>();
            assert!(controller.active.is_none());
            assert_eq!(controller.failed_revision, Some(1));
            assert_eq!(controller.session_counter, 0);
            assert_local_simulation_is_fenced(target.world_mut());
        }

        target
            .world_mut()
            .resource_mut::<UserModeState>()
            .request_embedded_match_for_test();
        assert_eq!(
            target
                .world()
                .resource::<UserModeState>()
                .match_request_revision(),
            2
        );
        reconcile_embedded_online_client(target.world_mut());

        let controller = target
            .world()
            .non_send_resource::<EmbeddedOnlineClientController>();
        assert!(controller.active.is_some());
        assert_eq!(controller.started_revision, Some(2));
        assert_eq!(controller.failed_revision, None);
        assert_eq!(
            target
                .world()
                .resource::<EmbeddedOnlineClientStatus>()
                .phase,
            EmbeddedOnlineClientPhase::Fighting
        );
    }

    #[test]
    fn runtime_failure_cannot_resume_local_or_restart_the_failed_revision() {
        let mut target = controller_target();
        target
            .world_mut()
            .insert_non_send_resource(EmbeddedOnlineClientController::default());
        reconcile_embedded_online_client(target.world_mut());
        {
            let controller = target
                .world_mut()
                .remove_non_send_resource::<EmbeddedOnlineClientController>()
                .unwrap();
            assert!(controller.active.is_some());
            assert_eq!(controller.started_revision, Some(1));
            target
                .world_mut()
                .insert_non_send_resource(EmbeddedOnlineClientController {
                    injected_runtime_failure: true,
                    ..controller
                });
        }

        drive_embedded_online_client(target.world_mut());

        let status = target.world().resource::<EmbeddedOnlineClientStatus>();
        assert_eq!(status.phase, EmbeddedOnlineClientPhase::Failed);
        assert_eq!(
            status.failure,
            Some(OnlineFailure {
                code: OnlineFailureCode::AuthorityLost,
                severity: OnlineFailureSeverity::MatchEnded,
                recovery: OnlineRecoveryAction::Retry,
                detail_code: EMBEDDED_RUNTIME_FAILURE_DETAIL,
            })
        );
        assert!(
            target
                .world()
                .resource::<UserModeState>()
                .network_match_requested()
        );
        assert_local_simulation_is_fenced(target.world_mut());
        assert_failure_is_visible_in_user_mode(target.world_mut(), "did not continue locally");

        for _ in 0..3 {
            reconcile_embedded_online_client(target.world_mut());
            let controller = target
                .world()
                .non_send_resource::<EmbeddedOnlineClientController>();
            assert!(controller.active.is_none());
            assert_eq!(controller.started_revision, Some(1));
            assert_eq!(controller.failed_revision, Some(1));
            assert_eq!(controller.session_counter, 1);
            assert_local_simulation_is_fenced(target.world_mut());
        }

        target
            .world_mut()
            .resource_mut::<UserModeState>()
            .request_embedded_match_for_test();
        reconcile_embedded_online_client(target.world_mut());

        let controller = target
            .world()
            .non_send_resource::<EmbeddedOnlineClientController>();
        assert!(controller.active.is_some());
        assert_eq!(controller.started_revision, Some(2));
        assert_eq!(controller.failed_revision, None);
        assert_eq!(controller.session_counter, 2);
        assert_eq!(
            target
                .world()
                .resource::<EmbeddedOnlineClientStatus>()
                .phase,
            EmbeddedOnlineClientPhase::Fighting
        );
    }
}
