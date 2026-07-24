//! Full-world predicted client composed from AFC's existing bounded engines.
//!
//! This type deliberately owns no transport. [`crate::local_loopback`] and future
//! UDP/Steam runners feed it verified wire messages; prediction, canonical-byte
//! baselines, rollback, re-simulation, hard resync, and presentation rewinds then
//! follow the same path for every transport.

use std::fmt;

use crate::local_loopback::{
    AppliedCanonicalSnapshot, ClientAuthorityOutcome, InitialSnapshotTarget,
};
use crate::network_codec::{StateDeltaAndAcks, StateHashAndAcks};
use crate::network_protocol::{
    CommittedInputRelay, InputFrame, MAX_INPUT_FRAMES_PER_WINDOW, MAX_SEATS, MatchId,
    MatchManifest, ProtocolValidationError, ResyncInputTail, ResyncReason, SeatId, SimTick,
    StateBaselineAck, StateHash,
};
use crate::rollback::{
    HardResyncReason, LateInputOutcome, NoopEventDiscard, NoopRollbackTiming, PredictionEngine,
    ReconcileOutcome, RollbackError, RollbackEventDiscard, RollbackTimingHook, RollbackWorld,
};
use crate::snapshot::{CanonicalSnapshot, SnapshotError};
use crate::state_sync::{
    ClientBaselineHistory, ClientDeltaOutcome, ClientStateSyncError, StateBaseline, StateSyncError,
};

pub const MAX_COMMITTED_RELAY_FRAMES: usize = MAX_SEATS * MAX_INPUT_FRAMES_PER_WINDOW;
/// Hash-verified snapshots are useful delta bases, but canonical encoding is a
/// measured client hot path. After the first post-sync verification, retain at
/// most one promoted baseline every two ticks (30 Hz at AFC's 60 Hz contract).
const MIN_HASH_BASELINE_PROMOTION_TICKS: u64 = 2;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PredictedClientMetrics {
    pub initial_syncs: u64,
    pub predicted_ticks: u64,
    pub hash_matches: u64,
    pub hash_baselines_promoted: u64,
    pub hash_mismatches_awaiting_state: u64,
    pub authoritative_corrections: u64,
    pub late_input_corrections: u64,
    pub hard_resync_requests: u64,
    pub hard_resyncs_applied: u64,
    pub stale_resync_baselines_accepted: u64,
    pub deltas_applied: u64,
    pub deltas_ignored: u64,
    pub committed_relay_frames: u64,
    pub committed_relay_corrections: u64,
    pub maximum_corrections_per_relay: u8,
}

#[derive(Debug)]
pub enum PredictedClientError<WorldError> {
    Protocol(ProtocolValidationError),
    StateSync(StateSyncError),
    ClientStateSync(ClientStateSyncError),
    Rollback(RollbackError<WorldError, SnapshotError>),
    NotInitialized,
    InitialSnapshotHashMismatch {
        snapshot: StateHash,
        world: StateHash,
    },
    ConflictingCommittedInput {
        tick: SimTick,
        seat: u8,
    },
    ConflictingAuthorityState {
        tick: SimTick,
        retained: StateHash,
        offered: StateHash,
    },
}

impl<WE: fmt::Debug> fmt::Display for PredictedClientError<WE> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "predicted client failed: {self:?}")
    }
}

impl<WE: fmt::Debug> std::error::Error for PredictedClientError<WE> {}

impl<WE> From<StateSyncError> for PredictedClientError<WE> {
    fn from(error: StateSyncError) -> Self {
        Self::StateSync(error)
    }
}

impl<WE> From<ProtocolValidationError> for PredictedClientError<WE> {
    fn from(error: ProtocolValidationError) -> Self {
        Self::Protocol(error)
    }
}

impl<WE> From<ClientStateSyncError> for PredictedClientError<WE> {
    fn from(error: ClientStateSyncError) -> Self {
        Self::ClientStateSync(error)
    }
}

/// A complete predicted canonical world plus bounded rollback and byte-baseline
/// histories. `D` is the presentation/event rewind hook and `T` is diagnostic
/// timing only; neither can affect canonical state.
pub struct PredictedClient<W, D = NoopEventDiscard, T = NoopRollbackTiming>
where
    W: RollbackWorld<Snapshot = CanonicalSnapshot>,
    D: RollbackEventDiscard,
    T: RollbackTimingHook,
{
    match_id: MatchId,
    world: W,
    prediction: Option<PredictionEngine<CanonicalSnapshot>>,
    baselines: ClientBaselineHistory,
    acknowledged_baseline: Option<StateBaseline>,
    history_capacity: usize,
    events: D,
    timing: T,
    awaiting_snapshot_tick: Option<SimTick>,
    future_committed: Vec<Option<FutureCommittedInputs>>,
    committed_coverage: Vec<Option<CommittedTickCoverage>>,
    required_seat_mask: u8,
    pending_state: Option<PendingAuthorityState>,
    latest_relay_tick: Option<SimTick>,
    relay_recoverable_oldest: Option<SimTick>,
    hard_resync_requested: bool,
    last_hard_resync_reason: Option<HardResyncReason>,
    metrics: PredictedClientMetrics,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FutureCommittedInputs {
    tick: SimTick,
    frames: [Option<InputFrame>; MAX_SEATS],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CommittedTickCoverage {
    tick: SimTick,
    seat_mask: u8,
    frames: [Option<InputFrame>; MAX_SEATS],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingAuthorityState {
    Hash(StateHashAndAcks),
    Delta(StateDeltaAndAcks),
}

impl PendingAuthorityState {
    const fn authority_tick(self) -> SimTick {
        match self {
            Self::Hash(message) => message.authority_tick,
            Self::Delta(message) => message.authority_tick,
        }
    }

    const fn state_hash(self) -> StateHash {
        match self {
            Self::Hash(message) => message.state_hash,
            Self::Delta(message) => message.state_hash,
        }
    }

    const fn is_delta(self) -> bool {
        matches!(self, Self::Delta(_))
    }
}

impl<W> PredictedClient<W, NoopEventDiscard, NoopRollbackTiming>
where
    W: RollbackWorld<Snapshot = CanonicalSnapshot>,
{
    pub fn new(
        world: W,
        match_id: MatchId,
        history_capacity: usize,
    ) -> Result<Self, PredictedClientError<W::Error>> {
        Self::with_hooks(
            world,
            match_id,
            history_capacity,
            NoopEventDiscard,
            NoopRollbackTiming,
        )
    }
}

impl<W, D, T> PredictedClient<W, D, T>
where
    W: RollbackWorld<Snapshot = CanonicalSnapshot>,
    D: RollbackEventDiscard,
    T: RollbackTimingHook,
{
    pub fn with_hooks(
        world: W,
        match_id: MatchId,
        history_capacity: usize,
        events: D,
        timing: T,
    ) -> Result<Self, PredictedClientError<W::Error>> {
        Ok(Self {
            match_id,
            world,
            prediction: None,
            baselines: ClientBaselineHistory::new(match_id, history_capacity)?,
            acknowledged_baseline: None,
            history_capacity,
            events,
            timing,
            awaiting_snapshot_tick: None,
            future_committed: vec![None; history_capacity],
            committed_coverage: vec![None; history_capacity],
            required_seat_mask: (1_u8 << MAX_SEATS) - 1,
            pending_state: None,
            latest_relay_tick: None,
            relay_recoverable_oldest: None,
            hard_resync_requested: false,
            last_hard_resync_reason: None,
            metrics: PredictedClientMetrics::default(),
        })
    }

    pub const fn world(&self) -> &W {
        &self.world
    }

    pub fn world_mut(&mut self) -> &mut W {
        &mut self.world
    }

    pub const fn events(&self) -> &D {
        &self.events
    }

    pub fn events_mut(&mut self) -> &mut D {
        &mut self.events
    }

    pub const fn metrics(&self) -> PredictedClientMetrics {
        self.metrics
    }

    pub fn prediction(&self) -> Option<&PredictionEngine<CanonicalSnapshot>> {
        self.prediction.as_ref()
    }

    pub fn predicted_tick(&self) -> Option<SimTick> {
        self.prediction
            .as_ref()
            .map(PredictionEngine::predicted_tick)
    }

    pub fn confirmed_tick(&self) -> Option<SimTick> {
        self.prediction
            .as_ref()
            .map(PredictionEngine::confirmed_tick)
    }

    /// Exact retained canonical state for presentation/result publication.
    /// This is a read-only history view and never rewinds the live world.
    pub fn snapshot_at(&self, tick: SimTick) -> Option<&CanonicalSnapshot> {
        self.prediction
            .as_ref()
            .and_then(|prediction| prediction.snapshot_at(tick))
    }

    pub const fn acknowledged_baseline(&self) -> Option<StateBaseline> {
        self.acknowledged_baseline
    }

    pub const fn last_hard_resync_reason(&self) -> Option<HardResyncReason> {
        self.last_hard_resync_reason
    }

    pub fn configure_manifest(
        &mut self,
        manifest: &MatchManifest,
    ) -> Result<(), PredictedClientError<W::Error>> {
        manifest.validate()?;
        if manifest.match_id != self.match_id {
            return Err(ProtocolValidationError::MatchMismatch.into());
        }
        self.required_seat_mask = manifest
            .ownership
            .as_slice()
            .iter()
            .fold(0_u8, |mask, assignment| mask | (1 << assignment.seat.get()));
        Ok(())
    }

    /// Seeds prediction's continuous input state from the reliable tail that is
    /// identity-bound to a just-applied snapshot. Only the newest record is a
    /// simulation boundary; older records are retained as committed coverage and
    /// are never replayed behind the confirmed snapshot.
    pub fn seed_resync_input_tail(
        &mut self,
        tail: &ResyncInputTail,
    ) -> Result<(), PredictedClientError<W::Error>> {
        tail.validate()?;
        if tail.match_id != self.match_id {
            return Err(ProtocolValidationError::MatchMismatch.into());
        }
        let prediction = self
            .prediction
            .as_ref()
            .ok_or(PredictedClientError::NotInitialized)?;
        if tail.snapshot_tick < prediction.confirmed_tick() {
            return Ok(());
        }
        if tail.snapshot_tick != prediction.confirmed_tick()
            || tail.snapshot_tick != prediction.predicted_tick()
        {
            return Err(ProtocolValidationError::InvalidTickWindow.into());
        }

        let mut boundary = std::array::from_fn(|seat| InputFrame {
            tick: tail.snapshot_tick,
            seat: SeatId::new(seat as u8)
                .expect("the fixed boundary array contains only valid seats"),
            ..InputFrame::default()
        });
        let mut seen = 0_u8;
        for window in tail.as_slice() {
            let newest = window
                .newest()
                .ok_or(ProtocolValidationError::EmptyInputWindow)?;
            let bit = 1 << newest.frame.seat.get();
            if self.required_seat_mask & bit == 0 || seen & bit != 0 {
                return Err(ProtocolValidationError::UnownedSeat.into());
            }
            seen |= bit;
            boundary[usize::from(newest.frame.seat.get())] = newest.frame;
            for record in window.as_slice().iter().rev() {
                self.mark_committed(record.frame)?;
            }
        }
        if seen != self.required_seat_mask {
            return Err(ProtocolValidationError::MissingFighterOwner.into());
        }
        self.prediction
            .as_mut()
            .expect("prediction was checked above")
            .seed_input_boundary(boundary)
            .map_err(|error| PredictedClientError::Rollback(RollbackError::Input(error)))?;
        self.latest_relay_tick = Some(tail.snapshot_tick);
        self.relay_recoverable_oldest = Some(tail.recent_input_start);
        Ok(())
    }

    pub fn predict_next(
        &mut self,
        mut provided: [Option<InputFrame>; MAX_SEATS],
    ) -> Result<SimTick, PredictedClientError<W::Error>> {
        let next_tick = self
            .prediction
            .as_ref()
            .ok_or(PredictedClientError::NotInitialized)?
            .predicted_tick()
            .next();
        let slot = self.future_slot(next_tick);
        if let Some(relayed) = self.future_committed[slot].take() {
            if relayed.tick == next_tick {
                for seat in 0..MAX_SEATS {
                    if relayed.frames[seat].is_some() {
                        provided[seat] = relayed.frames[seat];
                    }
                }
            } else {
                self.future_committed[slot] = Some(relayed);
            }
        }
        let prediction = self
            .prediction
            .as_mut()
            .ok_or(PredictedClientError::NotInitialized)?;
        let tick = prediction
            .predict_next(&mut self.world, provided)
            .map_err(PredictedClientError::Rollback)?;
        self.metrics.predicted_ticks = self.metrics.predicted_ticks.saturating_add(1);
        Ok(tick)
    }

    pub fn apply_late_input(
        &mut self,
        frame: InputFrame,
    ) -> Result<LateInputOutcome, PredictedClientError<W::Error>> {
        let prediction = self
            .prediction
            .as_mut()
            .ok_or(PredictedClientError::NotInitialized)?;
        let outcome = prediction
            .apply_late_input(&mut self.world, frame, &mut self.events, &mut self.timing)
            .map_err(PredictedClientError::Rollback)?;
        match outcome {
            LateInputOutcome::Corrected { .. } => {
                self.metrics.late_input_corrections =
                    self.metrics.late_input_corrections.saturating_add(1);
            }
            LateInputOutcome::HardResyncRequired(reason) => {
                let _ = self.hard_resync_outcome(reason);
            }
            LateInputOutcome::Unchanged { .. } => {}
        }
        Ok(outcome)
    }

    pub fn observe_committed_relay(
        &mut self,
        relay: &CommittedInputRelay,
    ) -> Result<ClientAuthorityOutcome, PredictedClientError<W::Error>> {
        relay.validate()?;
        if relay.match_id != self.match_id {
            return Err(ProtocolValidationError::MatchMismatch.into());
        }
        let prediction = self
            .prediction
            .as_ref()
            .ok_or(PredictedClientError::NotInitialized)?;
        let predicted_tick = prediction.predicted_tick();
        // A full-resync snapshot atomically replaces every rollback history at
        // its confirmed boundary. Unreliable committed relays sent before that
        // reliable transfer may arrive later across channels; their gameplay
        // effects and input tail are already represented by the snapshot.
        if relay.authority_tick <= prediction.confirmed_tick() {
            return Ok(ClientAuthorityOutcome::Ignored);
        }
        if relay.authority_tick.0.saturating_sub(predicted_tick.0)
            > crate::rollback::NORMAL_ROLLBACK_LIMIT_TICKS
        {
            return Ok(self.hard_resync_outcome(HardResyncReason::AuthorityAhead {
                authority_tick: relay.authority_tick,
                predicted_tick,
            }));
        }
        self.observe_relay_frontier(relay);

        let mut corrected = None;
        let mut observed_frames = 0_u8;
        let mut corrections = 0_u8;
        let confirmed_tick = self
            .prediction
            .as_ref()
            .expect("relay processing requires initialized prediction")
            .confirmed_tick();
        for offset in (0..MAX_INPUT_FRAMES_PER_WINDOW).rev() {
            for window in relay.as_slice() {
                let Some(record) = window.as_slice().get(offset) else {
                    continue;
                };
                observed_frames = observed_frames.saturating_add(1);
                // Redundant windows can straddle a full-resync boundary. The
                // accepted snapshot and its input tail already supersede every
                // frame at or before that confirmed tick, and rollback history
                // for those ticks was intentionally cleared.
                if record.frame.tick <= confirmed_tick {
                    continue;
                }
                self.mark_committed(record.frame)?;
                if record.frame.tick > predicted_tick {
                    self.store_future_committed(record.frame)?;
                    continue;
                }
                let outcome = self.apply_late_input(record.frame)?;
                match outcome {
                    LateInputOutcome::Corrected {
                        input_tick,
                        resimulated_through,
                        ..
                    } => {
                        corrections = corrections.saturating_add(1);
                        corrected = Some((input_tick, resimulated_through));
                    }
                    LateInputOutcome::HardResyncRequired(reason) => {
                        return Ok(self.hard_resync_outcome_without_increment(reason));
                    }
                    LateInputOutcome::Unchanged { .. } => {}
                }
            }
        }
        debug_assert!(usize::from(observed_frames) <= MAX_COMMITTED_RELAY_FRAMES);
        self.metrics.committed_relay_frames = self
            .metrics
            .committed_relay_frames
            .saturating_add(u64::from(observed_frames));
        self.metrics.committed_relay_corrections = self
            .metrics
            .committed_relay_corrections
            .saturating_add(u64::from(corrections));
        self.metrics.maximum_corrections_per_relay =
            self.metrics.maximum_corrections_per_relay.max(corrections);

        if let Some(tick) = self.first_unrecoverable_committed_gap() {
            return Ok(self.hard_resync_outcome(HardResyncReason::MissingInputHistory { tick }));
        }

        let relay_outcome = corrected.map_or(
            ClientAuthorityOutcome::Ignored,
            |(authoritative_tick, resimulated_through)| ClientAuthorityOutcome::Corrected {
                authoritative_tick,
                resimulated_through,
            },
        );
        let pending = self.process_pending_authority()?;
        Ok(
            if matches!(
                pending,
                ClientAuthorityOutcome::Passive | ClientAuthorityOutcome::Ignored
            ) {
                relay_outcome
            } else {
                pending
            },
        )
    }

    fn future_slot(&self, tick: SimTick) -> usize {
        tick.0 as usize % self.future_committed.len()
    }

    fn mark_committed(&mut self, frame: InputFrame) -> Result<(), PredictedClientError<W::Error>> {
        let slot = self.future_slot(frame.tick);
        let coverage = self.committed_coverage[slot].get_or_insert(CommittedTickCoverage {
            tick: frame.tick,
            seat_mask: 0,
            frames: [None; MAX_SEATS],
        });
        if coverage.tick != frame.tick {
            *coverage = CommittedTickCoverage {
                tick: frame.tick,
                seat_mask: 0,
                frames: [None; MAX_SEATS],
            };
        }
        let seat = usize::from(frame.seat.get());
        if coverage.frames[seat].is_some_and(|retained| retained != frame) {
            return Err(PredictedClientError::ConflictingCommittedInput {
                tick: frame.tick,
                seat: frame.seat.get(),
            });
        }
        coverage.frames[seat] = Some(frame);
        coverage.seat_mask |= 1 << frame.seat.get();
        Ok(())
    }

    fn observe_relay_frontier(&mut self, relay: &CommittedInputRelay) {
        if self
            .latest_relay_tick
            .is_some_and(|latest| relay.authority_tick < latest)
        {
            return;
        }
        let mut seen = 0_u8;
        let mut recoverable_oldest = SimTick::ZERO;
        for window in relay.as_slice() {
            let Some(newest) = window.newest() else {
                continue;
            };
            let bit = 1 << newest.frame.seat.get();
            if self.required_seat_mask & bit == 0 {
                continue;
            }
            seen |= bit;
            let oldest = window
                .as_slice()
                .last()
                .expect("validated committed windows are non-empty")
                .frame
                .tick;
            recoverable_oldest = recoverable_oldest.max(oldest);
        }
        if seen & self.required_seat_mask == self.required_seat_mask {
            self.latest_relay_tick = Some(relay.authority_tick);
            self.relay_recoverable_oldest = Some(recoverable_oldest);
        }
    }

    fn first_unrecoverable_committed_gap(&self) -> Option<SimTick> {
        let prediction = self.prediction.as_ref()?;
        let oldest = self.relay_recoverable_oldest?;
        let frontier = self.latest_relay_tick?.min(prediction.predicted_tick());
        let confirmed = prediction.confirmed_tick();
        if frontier <= confirmed {
            return None;
        }
        ((confirmed.0 + 1)..=frontier.0).map(SimTick).find(|tick| {
            *tick < oldest
                && !self.committed_coverage[self.future_slot(*tick)].is_some_and(|coverage| {
                    coverage.tick == *tick
                        && coverage.seat_mask & self.required_seat_mask == self.required_seat_mask
                })
        })
    }

    fn committed_inputs_cover_through(&self, target: SimTick) -> bool {
        let Some(prediction) = self.prediction.as_ref() else {
            return false;
        };
        let confirmed = prediction.confirmed_tick();
        if target <= confirmed {
            return true;
        }
        if target > prediction.predicted_tick()
            || target.0 - confirmed.0 > self.committed_coverage.len() as u64
        {
            return false;
        }
        ((confirmed.0 + 1)..=target.0).all(|tick| {
            let tick = SimTick(tick);
            self.committed_coverage[self.future_slot(tick)].is_some_and(|coverage| {
                coverage.tick == tick
                    && coverage.seat_mask & self.required_seat_mask == self.required_seat_mask
            })
        })
    }

    pub fn process_pending_authority(
        &mut self,
    ) -> Result<ClientAuthorityOutcome, PredictedClientError<W::Error>> {
        let Some(pending) = self.pending_state else {
            return Ok(ClientAuthorityOutcome::Passive);
        };
        if !self.committed_inputs_cover_through(pending.authority_tick()) {
            return Ok(ClientAuthorityOutcome::Passive);
        }
        self.pending_state = None;
        match pending {
            PendingAuthorityState::Hash(message) => self.reconcile_hash(&message),
            PendingAuthorityState::Delta(message) => self.reconcile_delta(&message),
        }
    }

    fn store_future_committed(
        &mut self,
        frame: InputFrame,
    ) -> Result<(), PredictedClientError<W::Error>> {
        let slot = self.future_slot(frame.tick);
        let entry = self.future_committed[slot].get_or_insert(FutureCommittedInputs {
            tick: frame.tick,
            frames: [None; MAX_SEATS],
        });
        if entry.tick != frame.tick {
            *entry = FutureCommittedInputs {
                tick: frame.tick,
                frames: [None; MAX_SEATS],
            };
        }
        let seat = usize::from(frame.seat.get());
        if entry.frames[seat].is_some_and(|retained| retained != frame) {
            return Err(PredictedClientError::ConflictingCommittedInput {
                tick: frame.tick,
                seat: frame.seat.get(),
            });
        }
        entry.frames[seat] = Some(frame);
        Ok(())
    }

    fn retain_pending_state(
        &mut self,
        candidate: PendingAuthorityState,
    ) -> Result<(), PredictedClientError<W::Error>> {
        let Some(retained) = self.pending_state else {
            self.pending_state = Some(candidate);
            return Ok(());
        };
        if candidate.authority_tick() < retained.authority_tick() {
            return Ok(());
        }
        if candidate.authority_tick() > retained.authority_tick() {
            self.pending_state = Some(candidate);
            return Ok(());
        }
        if candidate.state_hash() != retained.state_hash() {
            return Err(PredictedClientError::ConflictingAuthorityState {
                tick: candidate.authority_tick(),
                retained: retained.state_hash(),
                offered: candidate.state_hash(),
            });
        }
        if candidate.is_delta() || !retained.is_delta() {
            self.pending_state = Some(candidate);
        }
        Ok(())
    }

    fn discard_replication_through(&mut self, boundary: SimTick, rebase_prediction: bool) {
        if self
            .pending_state
            .is_some_and(|pending| pending.authority_tick() <= boundary)
        {
            self.pending_state = None;
        }
        for coverage in &mut self.committed_coverage {
            if coverage.is_some_and(|retained| retained.tick <= boundary) {
                *coverage = None;
            }
        }
        if rebase_prediction {
            self.future_committed.fill(None);
            for (future, coverage) in self
                .future_committed
                .iter_mut()
                .zip(self.committed_coverage.iter().copied())
            {
                let Some(coverage) = coverage else {
                    continue;
                };
                *future = Some(FutureCommittedInputs {
                    tick: coverage.tick,
                    frames: coverage.frames,
                });
            }
        } else {
            for future in &mut self.future_committed {
                if future.is_some_and(|retained| retained.tick <= boundary) {
                    *future = None;
                }
            }
        }
        if self
            .latest_relay_tick
            .is_some_and(|latest| latest <= boundary)
        {
            self.latest_relay_tick = None;
            self.relay_recoverable_oldest = None;
        } else if let Some(oldest) = self.relay_recoverable_oldest {
            self.relay_recoverable_oldest = Some(oldest.max(boundary.next()));
        }
    }

    fn install_initial(
        &mut self,
        snapshot: &CanonicalSnapshot,
    ) -> Result<AppliedCanonicalSnapshot, PredictedClientError<W::Error>> {
        self.events.discard_after(snapshot.header.tick);
        self.world
            .restore_snapshot(snapshot)
            .map_err(|error| PredictedClientError::Rollback(RollbackError::World(error)))?;
        let snapshot_hash =
            StateHash(snapshot.canonical_hash().map_err(|error| {
                PredictedClientError::Rollback(RollbackError::SnapshotHash(error))
            })?);
        let world_hash = StateHash(
            self.world
                .state_hash()
                .map_err(|error| PredictedClientError::Rollback(RollbackError::World(error)))?,
        );
        if snapshot_hash != world_hash {
            return Err(PredictedClientError::InitialSnapshotHashMismatch {
                snapshot: snapshot_hash,
                world: world_hash,
            });
        }
        let baseline = self.baselines.reset_to_snapshot(snapshot)?;
        self.prediction = Some(
            PredictionEngine::new(&self.world, self.history_capacity)
                .map_err(PredictedClientError::Rollback)?,
        );
        self.acknowledged_baseline = Some(baseline);
        self.awaiting_snapshot_tick = None;
        self.future_committed.fill(None);
        self.committed_coverage.fill(None);
        self.pending_state = None;
        self.latest_relay_tick = None;
        self.relay_recoverable_oldest = None;
        self.hard_resync_requested = false;
        self.metrics.initial_syncs = self.metrics.initial_syncs.saturating_add(1);
        Ok(AppliedCanonicalSnapshot {
            tick: snapshot.header.tick,
            hash: snapshot_hash,
        })
    }

    fn apply_full_resync(
        &mut self,
        snapshot: &CanonicalSnapshot,
    ) -> Result<AppliedCanonicalSnapshot, PredictedClientError<W::Error>> {
        let hash =
            StateHash(snapshot.canonical_hash().map_err(|error| {
                PredictedClientError::Rollback(RollbackError::SnapshotHash(error))
            })?);
        let snapshot_tick = snapshot.header.tick;
        if self
            .prediction
            .as_ref()
            .is_some_and(|prediction| snapshot_tick < prediction.confirmed_tick())
        {
            // A reliable full snapshot can be overtaken by newer, independently
            // verified state/hash packets while crossing channels. Never move a
            // confirmed world backward. If the transfer advances the delta base,
            // retain its verified bytes as the new baseline; otherwise receipt is
            // idempotent because an even newer baseline is already acknowledged.
            if self
                .acknowledged_baseline
                .is_none_or(|acknowledged| snapshot_tick > acknowledged.tick)
            {
                self.acknowledged_baseline = Some(self.baselines.reset_to_snapshot(snapshot)?);
            }
            self.awaiting_snapshot_tick = None;
            let confirmed = self
                .prediction
                .as_ref()
                .expect("stale resync requires initialized prediction")
                .confirmed_tick();
            self.discard_replication_through(confirmed, false);
            self.hard_resync_requested = false;
            self.metrics.stale_resync_baselines_accepted = self
                .metrics
                .stale_resync_baselines_accepted
                .saturating_add(1);
            return Ok(AppliedCanonicalSnapshot {
                tick: snapshot_tick,
                hash,
            });
        }
        if let Some(prediction) = self.prediction.as_mut() {
            prediction
                .apply_hard_resync(
                    &mut self.world,
                    snapshot,
                    hash.0,
                    &mut self.events,
                    &mut self.timing,
                )
                .map_err(PredictedClientError::Rollback)?;
        } else {
            return self.install_initial(snapshot);
        }
        self.acknowledged_baseline = Some(self.baselines.reset_to_snapshot(snapshot)?);
        self.awaiting_snapshot_tick = None;
        self.discard_replication_through(snapshot_tick, true);
        self.hard_resync_requested = false;
        self.metrics.hard_resyncs_applied = self.metrics.hard_resyncs_applied.saturating_add(1);
        Ok(AppliedCanonicalSnapshot {
            tick: snapshot.header.tick,
            hash,
        })
    }

    fn observe_hash(
        &mut self,
        message: &StateHashAndAcks,
    ) -> Result<ClientAuthorityOutcome, PredictedClientError<W::Error>> {
        if !self.committed_inputs_cover_through(message.authority_tick) {
            self.retain_pending_state(PendingAuthorityState::Hash(*message))?;
            return Ok(ClientAuthorityOutcome::AwaitingCommittedInputs {
                tick: message.authority_tick,
            });
        }
        self.reconcile_hash(message)
    }

    fn reconcile_hash(
        &mut self,
        message: &StateHashAndAcks,
    ) -> Result<ClientAuthorityOutcome, PredictedClientError<W::Error>> {
        let predicted_hash_matches = {
            let prediction = self
                .prediction
                .as_ref()
                .ok_or(PredictedClientError::NotInitialized)?;
            prediction.predicted_hash(message.authority_tick) == Some(message.state_hash.0)
        };
        if predicted_hash_matches {
            let outcome = self
                .prediction
                .as_mut()
                .expect("matching snapshot requires initialized prediction")
                .reconcile(
                    &mut self.world,
                    message.authority_tick,
                    message.state_hash.0,
                    None,
                    &mut self.events,
                    &mut self.timing,
                )
                .map_err(PredictedClientError::Rollback)?;
            self.metrics.hash_matches = self.metrics.hash_matches.saturating_add(1);
            let mapped = self.map_reconcile(outcome);
            if matches!(mapped, ClientAuthorityOutcome::Matched { .. })
                && self.should_promote_verified_baseline(message.authority_tick)
            {
                // A canonical predicted snapshot whose exact tick and hash were
                // verified by the authority is safe to use as the next byte
                // delta baseline. Acknowledging that bounded local copy keeps
                // the authority close to the client without unsolicited full
                // transfers when a three-tick delta would be too dense.
                let candidate = StateBaseline::new(message.authority_tick, message.state_hash);
                let baseline = if self.baselines.contains(candidate) {
                    candidate
                } else {
                    let snapshot = self
                        .prediction
                        .as_ref()
                        .and_then(|prediction| prediction.snapshot_at(message.authority_tick))
                        .expect("a matched predicted hash retains its canonical snapshot");
                    self.baselines.install_snapshot(snapshot)?
                };
                debug_assert_eq!(baseline.tick, message.authority_tick);
                debug_assert_eq!(baseline.hash, message.state_hash);
                if self
                    .acknowledged_baseline
                    .is_none_or(|acknowledged| baseline.tick >= acknowledged.tick)
                {
                    self.acknowledged_baseline = Some(baseline);
                }
                self.metrics.hash_baselines_promoted =
                    self.metrics.hash_baselines_promoted.saturating_add(1);
                if self
                    .awaiting_snapshot_tick
                    .is_some_and(|awaiting| awaiting <= baseline.tick)
                {
                    self.awaiting_snapshot_tick = None;
                }
            }
            return Ok(mapped);
        }

        let prediction = self
            .prediction
            .as_mut()
            .ok_or(PredictedClientError::NotInitialized)?;
        if prediction.predicted_hash(message.authority_tick).is_some() {
            self.awaiting_snapshot_tick = Some(message.authority_tick);
            self.metrics.hash_mismatches_awaiting_state = self
                .metrics
                .hash_mismatches_awaiting_state
                .saturating_add(1);
            return Ok(ClientAuthorityOutcome::AwaitingAuthoritativeSnapshot {
                tick: message.authority_tick,
            });
        }

        let outcome = prediction
            .reconcile(
                &mut self.world,
                message.authority_tick,
                message.state_hash.0,
                None,
                &mut self.events,
                &mut self.timing,
            )
            .map_err(PredictedClientError::Rollback)?;
        Ok(self.map_reconcile(outcome))
    }

    fn should_promote_verified_baseline(&self, tick: SimTick) -> bool {
        self.acknowledged_baseline.is_none_or(|acknowledged| {
            tick > acknowledged.tick
                && (acknowledged.tick == SimTick::ZERO
                    || tick.get().saturating_sub(acknowledged.tick.get())
                        >= MIN_HASH_BASELINE_PROMOTION_TICKS)
        })
    }

    fn observe_delta(
        &mut self,
        message: &StateDeltaAndAcks,
    ) -> Result<ClientAuthorityOutcome, PredictedClientError<W::Error>> {
        if !self.committed_inputs_cover_through(message.authority_tick) {
            self.retain_pending_state(PendingAuthorityState::Delta(*message))?;
            return Ok(ClientAuthorityOutcome::AwaitingCommittedInputs {
                tick: message.authority_tick,
            });
        }
        self.reconcile_delta(message)
    }

    fn reconcile_delta(
        &mut self,
        message: &StateDeltaAndAcks,
    ) -> Result<ClientAuthorityOutcome, PredictedClientError<W::Error>> {
        let outcome = match self.baselines.apply_delta(message) {
            Ok(outcome) => outcome,
            Err(ClientStateSyncError::BaselineMissing(missing)) => {
                // State deltas are intentionally carried on a different channel
                // from reliable full-resync snapshots. A delta that was already
                // in flight can therefore arrive after a hard resync has reset
                // the bounded baseline history. If the client has already
                // acknowledged a newer baseline, this packet is obsolete even
                // when its target tick is newer; the authority will rebuild from
                // that explicit acknowledgement. Otherwise the missing base is
                // recoverable via another full snapshot. Every integrity or
                // identity failure still propagates as a fatal validation error.
                if self
                    .acknowledged_baseline
                    .is_some_and(|acknowledged| missing.tick < acknowledged.tick)
                {
                    self.metrics.deltas_ignored = self.metrics.deltas_ignored.saturating_add(1);
                    return Ok(ClientAuthorityOutcome::Ignored);
                }
                return Ok(
                    self.hard_resync_outcome(HardResyncReason::SnapshotHistoryExpired {
                        requested_tick: missing.tick,
                        oldest_available: None,
                    }),
                );
            }
            Err(error) => return Err(error.into()),
        };
        let ClientDeltaOutcome::Applied(applied) = outcome else {
            self.metrics.deltas_ignored = self.metrics.deltas_ignored.saturating_add(1);
            return Ok(ClientAuthorityOutcome::Ignored);
        };
        self.metrics.deltas_applied = self.metrics.deltas_applied.saturating_add(1);
        let prediction = self
            .prediction
            .as_mut()
            .ok_or(PredictedClientError::NotInitialized)?;
        let reconciled = prediction
            .reconcile(
                &mut self.world,
                applied.baseline.tick,
                applied.baseline.hash.0,
                Some(&applied.snapshot),
                &mut self.events,
                &mut self.timing,
            )
            .map_err(PredictedClientError::Rollback)?;
        let mapped = self.map_reconcile(reconciled);
        if matches!(
            mapped,
            ClientAuthorityOutcome::Matched { .. }
                | ClientAuthorityOutcome::Corrected { .. }
                | ClientAuthorityOutcome::Ignored
        ) {
            self.acknowledged_baseline = Some(applied.baseline);
            self.awaiting_snapshot_tick = None;
        }
        Ok(mapped)
    }

    fn map_reconcile(&mut self, outcome: ReconcileOutcome) -> ClientAuthorityOutcome {
        match outcome {
            ReconcileOutcome::Matched { tick, .. } => ClientAuthorityOutcome::Matched { tick },
            ReconcileOutcome::Corrected {
                authoritative_tick,
                resimulated_through,
                ..
            } => {
                self.metrics.authoritative_corrections =
                    self.metrics.authoritative_corrections.saturating_add(1);
                ClientAuthorityOutcome::Corrected {
                    authoritative_tick,
                    resimulated_through,
                }
            }
            ReconcileOutcome::StaleAuthority { .. } => ClientAuthorityOutcome::Ignored,
            ReconcileOutcome::HardResyncRequired(reason) => self.hard_resync_outcome(reason),
        }
    }

    fn hard_resync_outcome(&mut self, reason: HardResyncReason) -> ClientAuthorityOutcome {
        self.last_hard_resync_reason = Some(reason);
        if !self.hard_resync_requested {
            self.metrics.hard_resync_requests = self.metrics.hard_resync_requests.saturating_add(1);
            self.hard_resync_requested = true;
        }
        self.hard_resync_outcome_without_increment(reason)
    }

    fn hard_resync_outcome_without_increment(
        &self,
        reason: HardResyncReason,
    ) -> ClientAuthorityOutcome {
        let prediction = self
            .prediction
            .as_ref()
            .expect("hard-resync outcomes require an initialized prediction engine");
        let last_confirmed_tick = prediction.confirmed_tick();
        let last_confirmed_hash = StateHash(
            prediction
                .predicted_hash(last_confirmed_tick)
                .unwrap_or_default(),
        );
        let reason = match reason {
            HardResyncReason::SnapshotHistoryExpired { .. }
            | HardResyncReason::MissingInputHistory { .. } => ResyncReason::HistoryExpired,
            _ => ResyncReason::HashMismatch,
        };
        ClientAuthorityOutcome::HardResyncRequired {
            reason,
            last_confirmed_tick,
            last_confirmed_hash,
        }
    }
}

impl<W, D, T> InitialSnapshotTarget for PredictedClient<W, D, T>
where
    W: RollbackWorld<Snapshot = CanonicalSnapshot>,
    D: RollbackEventDiscard,
    T: RollbackTimingHook,
{
    type Error = PredictedClientError<W::Error>;

    fn apply_initial_snapshot(
        &mut self,
        snapshot: &CanonicalSnapshot,
    ) -> Result<AppliedCanonicalSnapshot, Self::Error> {
        self.install_initial(snapshot)
    }

    fn configure_match(&mut self, manifest: &MatchManifest) -> Result<(), Self::Error> {
        self.configure_manifest(manifest)
    }

    fn state_baseline_ack(&self) -> Option<StateBaselineAck> {
        self.acknowledged_baseline.map(Into::into)
    }

    fn observe_authority_hash(
        &mut self,
        message: &StateHashAndAcks,
    ) -> Result<ClientAuthorityOutcome, Self::Error> {
        self.observe_hash(message)
    }

    fn observe_authority_delta(
        &mut self,
        message: &StateDeltaAndAcks,
    ) -> Result<ClientAuthorityOutcome, Self::Error> {
        self.observe_delta(message)
    }

    fn observe_committed_inputs(
        &mut self,
        message: &CommittedInputRelay,
    ) -> Result<ClientAuthorityOutcome, Self::Error> {
        self.observe_committed_relay(message)
    }

    fn seed_resync_input_tail(&mut self, tail: &ResyncInputTail) -> Result<(), Self::Error> {
        PredictedClient::seed_resync_input_tail(self, tail)
    }

    fn poll_authority(&mut self) -> Result<ClientAuthorityOutcome, Self::Error> {
        self.process_pending_authority()
    }

    fn apply_resync_snapshot(
        &mut self,
        snapshot: &CanonicalSnapshot,
    ) -> Result<AppliedCanonicalSnapshot, Self::Error> {
        self.apply_full_resync(snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::determinism::{FighterId, SimEntityKind};
    use crate::network_protocol::{
        CommittedInputRecord, CommittedInputSource, CommittedSeatInputWindow, InputButtons,
        InputSequence, PeerId, QuantizedAxis, ResyncBegin, SeatId, TransferId,
    };
    use crate::snapshot::{
        ArenaRuntimeSnapshot, FighterSnapshot, MatchPhaseSnapshot, MatchStateSnapshot,
        MatchStatsSnapshot, PoolAllocatorSnapshot, SnapshotHeader,
    };

    const MATCH_BYTES: [u8; 16] = *b"predicted-test-1";

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum WorldError {
        TickGap,
        WrongSeat,
        Snapshot(SnapshotError),
    }

    #[derive(Clone)]
    struct CounterWorld {
        snapshot: CanonicalSnapshot,
    }

    impl CounterWorld {
        fn initial() -> Self {
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
                    header: SnapshotHeader::new(1, 1, 0xAFC0, MATCH_BYTES, SimTick::ZERO, 7),
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
            }
        }
    }

    impl RollbackWorld for CounterWorld {
        type Snapshot = CanonicalSnapshot;
        type Error = WorldError;

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
                return Err(WorldError::TickGap);
            }
            for (seat, frame) in inputs.iter().enumerate() {
                if frame.tick != tick || usize::from(frame.seat.get()) != seat {
                    return Err(WorldError::WrongSeat);
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
            self.snapshot.canonical_hash().map_err(WorldError::Snapshot)
        }
    }

    #[derive(Default)]
    struct DiscardLog(Vec<SimTick>);

    impl RollbackEventDiscard for DiscardLog {
        fn discard_after(&mut self, retained_through: SimTick) {
            self.0.push(retained_through);
        }
    }

    fn match_id() -> MatchId {
        MatchId::new(MATCH_BYTES).unwrap()
    }

    fn frame(tick: u64, seat: usize, movement: i8) -> InputFrame {
        InputFrame {
            tick: SimTick(tick),
            seat: SeatId::new(seat as u8).unwrap(),
            movement_x: QuantizedAxis::new(movement).unwrap(),
            sequence: InputSequence(tick as u16),
            ..InputFrame::default()
        }
    }

    fn known_frames(tick: u64) -> [Option<InputFrame>; MAX_SEATS] {
        std::array::from_fn(|seat| Some(frame(tick, seat, seat as i8)))
    }

    fn relay(record: CommittedInputRecord) -> CommittedInputRelay {
        let tick = record.frame.tick;
        let window = CommittedSeatInputWindow::from_newest_first(&[record]).unwrap();
        CommittedInputRelay::new(match_id(), tick, &[window]).unwrap()
    }

    fn full_relay(frames: [InputFrame; MAX_SEATS]) -> CommittedInputRelay {
        let tick = frames[0].tick;
        let windows = std::array::from_fn::<_, MAX_SEATS, _>(|seat| {
            CommittedSeatInputWindow::from_newest_first(&[CommittedInputRecord {
                frame: frames[seat],
                fighter: FighterId::new(seat as u8).unwrap(),
                source: CommittedInputSource::Peer(PeerId::new(9).unwrap()),
            }])
            .unwrap()
        });
        CommittedInputRelay::new(match_id(), tick, &windows).unwrap()
    }

    fn redundant_full_relay(newest_tick: u64) -> CommittedInputRelay {
        let windows = std::array::from_fn::<_, MAX_SEATS, _>(|seat| {
            let records = std::array::from_fn::<_, MAX_INPUT_FRAMES_PER_WINDOW, _>(|offset| {
                let tick = newest_tick - offset as u64;
                CommittedInputRecord {
                    frame: frame(tick, seat, seat as i8),
                    fighter: FighterId::new(seat as u8).unwrap(),
                    source: CommittedInputSource::Peer(PeerId::new(9).unwrap()),
                }
            });
            CommittedSeatInputWindow::from_newest_first(&records).unwrap()
        });
        CommittedInputRelay::new(match_id(), SimTick(newest_tick), &windows).unwrap()
    }

    fn initialized_client() -> PredictedClient<CounterWorld, DiscardLog> {
        let initial = CounterWorld::initial().snapshot;
        let mut client = PredictedClient::with_hooks(
            CounterWorld::initial(),
            match_id(),
            32,
            DiscardLog::default(),
            NoopRollbackTiming,
        )
        .unwrap();
        client.apply_initial_snapshot(&initial).unwrap();
        client
    }

    fn authority_through(tick: u64) -> CounterWorld {
        let mut authority = CounterWorld::initial();
        for value in 1..=tick {
            authority
                .step(SimTick(value), &known_frames(value).map(Option::unwrap))
                .unwrap();
        }
        authority
    }

    fn exact_delta_through(tick: u64) -> StateDeltaAndAcks {
        use crate::state_sync::{AuthorityDeltaOutcome, AuthoritySnapshotHistory};

        let initial = CounterWorld::initial().capture_snapshot().unwrap();
        let target = authority_through(tick).capture_snapshot().unwrap();
        let mut history = AuthoritySnapshotHistory::new(match_id(), 8).unwrap();
        let base = history.record_snapshot(&initial).unwrap();
        history.record_snapshot(&target).unwrap();
        match history.build_latest_delta(base, &[]).unwrap() {
            AuthorityDeltaOutcome::Delta(message) => message,
            other => panic!("expected exact authority delta, got {other:?}"),
        }
    }

    #[test]
    fn resync_tail_preserves_held_input_into_first_post_snapshot_prediction() {
        let mut client = initialized_client();
        let mut authority = CounterWorld::initial();
        for tick in 1..=5 {
            authority
                .step(SimTick(tick), &known_frames(tick).map(Option::unwrap))
                .unwrap();
        }
        let snapshot = authority.capture_snapshot().unwrap();
        client.apply_resync_snapshot(&snapshot).unwrap();
        let begin = ResyncBegin {
            match_id: match_id(),
            transfer_id: TransferId::new(1).unwrap(),
            snapshot_tick: SimTick(5),
            snapshot_hash: StateHash(snapshot.canonical_hash().unwrap()),
            snapshot_bytes: 1,
            chunk_count: 1,
            recent_input_start: SimTick(1),
            recent_input_end: SimTick(5),
        };
        let windows = std::array::from_fn::<_, MAX_SEATS, _>(|seat| {
            let records = std::array::from_fn::<_, 5, _>(|offset| {
                let mut frame = frame(5 - offset as u64, seat, 40 + seat as i8);
                frame.held_buttons = InputButtons::new(InputButtons::GUARD).unwrap();
                frame.pressed_buttons = if offset == 0 {
                    InputButtons::new(InputButtons::GUARD).unwrap()
                } else {
                    InputButtons::default()
                };
                CommittedInputRecord {
                    frame,
                    fighter: FighterId::new(seat as u8).unwrap(),
                    source: CommittedInputSource::MissingSubstitute,
                }
            });
            CommittedSeatInputWindow::from_newest_first(&records).unwrap()
        });
        let tail = ResyncInputTail::new(&begin, &windows).unwrap();
        client.seed_resync_input_tail(&tail).unwrap();
        client.predict_next([None; MAX_SEATS]).unwrap();

        let first = client
            .prediction()
            .unwrap()
            .inputs_at(SimTick(6))
            .expect("first post-resync prediction is retained");
        for seat in 0..MAX_SEATS {
            let frame = first.frame(SeatId::new(seat as u8).unwrap());
            assert_eq!(frame.movement_x.get(), 40 + seat as i8);
            assert_eq!(frame.held_buttons.bits(), InputButtons::GUARD);
            assert_eq!(frame.pressed_buttons.bits(), 0);
            assert_eq!(frame.released_buttons.bits(), 0);
        }
    }

    #[test]
    fn matching_authority_hash_confirms_without_mutating_prediction() {
        let mut client = initialized_client();
        let inputs = known_frames(1).map(Option::unwrap);
        client.predict_next(inputs.map(Some)).unwrap();
        client.observe_committed_relay(&full_relay(inputs)).unwrap();
        let hash = StateHash(client.world().state_hash().unwrap());
        let state = StateHashAndAcks::new(match_id(), SimTick(1), hash, &[]).unwrap();

        assert_eq!(
            client.observe_authority_hash(&state).unwrap(),
            ClientAuthorityOutcome::Matched { tick: SimTick(1) }
        );
        assert_eq!(client.confirmed_tick(), Some(SimTick(1)));
        assert_eq!(client.world().state_hash().unwrap(), hash.0);
        assert_eq!(client.state_baseline_ack().unwrap().tick, SimTick(1));
        assert_eq!(client.state_baseline_ack().unwrap().hash, hash);
    }

    #[test]
    fn prediction_ahead_hash_match_promotes_a_baseline_for_the_next_delta() {
        use crate::state_sync::{AuthorityDeltaOutcome, AuthoritySnapshotHistory};

        let mut client = initialized_client();
        let mut authority = CounterWorld::initial();
        let mut tick_one_snapshot = None;
        for tick in 1..=3 {
            let inputs = known_frames(tick).map(Option::unwrap);
            client.predict_next(inputs.map(Some)).unwrap();
            client.observe_committed_relay(&full_relay(inputs)).unwrap();
            authority.step(SimTick(tick), &inputs).unwrap();
            if tick == 1 {
                tick_one_snapshot = Some(authority.capture_snapshot().unwrap());
            }
        }

        let tick_one = tick_one_snapshot.unwrap();
        let tick_one_hash = StateHash(tick_one.canonical_hash().unwrap());
        let state = StateHashAndAcks::new(match_id(), SimTick(1), tick_one_hash, &[]).unwrap();
        assert_eq!(
            client.observe_authority_hash(&state).unwrap(),
            ClientAuthorityOutcome::Matched { tick: SimTick(1) }
        );
        assert_eq!(client.predicted_tick(), Some(SimTick(3)));
        let promoted = client.state_baseline_ack().unwrap();
        assert_eq!(promoted.tick, SimTick(1));
        assert_eq!(promoted.hash, tick_one_hash);

        let target = authority.capture_snapshot().unwrap();
        let mut history = AuthoritySnapshotHistory::new(match_id(), 8).unwrap();
        assert_eq!(history.record_snapshot(&tick_one).unwrap(), promoted.into());
        history.record_snapshot(&target).unwrap();
        let delta = match history.build_latest_delta(promoted.into(), &[]).unwrap() {
            AuthorityDeltaOutcome::Delta(delta) => delta,
            other => panic!("promoted baseline did not build a delta: {other:?}"),
        };
        assert!(matches!(
            client.observe_authority_delta(&delta).unwrap(),
            ClientAuthorityOutcome::Matched { tick: SimTick(3) }
        ));
        assert_eq!(client.confirmed_tick(), Some(SimTick(3)));
    }

    #[test]
    fn hash_mismatch_never_promotes_the_predicted_snapshot() {
        let mut client = initialized_client();
        let initial_baseline = client.state_baseline_ack().unwrap();
        let inputs = known_frames(1).map(Option::unwrap);
        client.predict_next(inputs.map(Some)).unwrap();
        client.observe_committed_relay(&full_relay(inputs)).unwrap();
        let mismatched = StateHash(client.world().state_hash().unwrap() ^ 1);
        let state = StateHashAndAcks::new(match_id(), SimTick(1), mismatched, &[]).unwrap();

        assert_eq!(
            client.observe_authority_hash(&state).unwrap(),
            ClientAuthorityOutcome::AwaitingAuthoritativeSnapshot { tick: SimTick(1) }
        );
        assert_eq!(client.state_baseline_ack().unwrap(), initial_baseline);
    }

    #[test]
    fn late_remote_committed_input_rolls_back_and_discards_presentation() {
        let mut client = initialized_client();
        client.events_mut().0.clear();
        let mut tick_one = known_frames(1);
        tick_one[1] = None;
        client.predict_next(tick_one).unwrap();
        let mut tick_two = known_frames(2);
        tick_two[1] = None;
        client.predict_next(tick_two).unwrap();

        let authoritative = frame(1, 1, 45);
        let outcome = client
            .observe_committed_relay(&relay(CommittedInputRecord {
                frame: authoritative,
                fighter: FighterId::new(1).unwrap(),
                source: CommittedInputSource::Peer(PeerId::new(9).unwrap()),
            }))
            .unwrap();
        assert!(matches!(
            outcome,
            ClientAuthorityOutcome::Corrected {
                authoritative_tick: SimTick(1),
                resimulated_through: SimTick(2),
            }
        ));
        assert_eq!(client.events().0, vec![SimTick::ZERO]);
        assert_eq!(
            client
                .prediction()
                .unwrap()
                .inputs_at(SimTick(2))
                .unwrap()
                .frame(SeatId::new(1).unwrap())
                .movement_x,
            authoritative.movement_x
        );
        assert_eq!(client.last_hard_resync_reason(), None);
    }

    #[test]
    fn full_resync_boundary_ignores_an_overtaken_older_committed_relay() {
        let mut client = initialized_client();
        let mut authority = CounterWorld::initial();
        for tick in 1..=3 {
            authority
                .step(SimTick(tick), &known_frames(tick).map(Option::unwrap))
                .unwrap();
        }
        let snapshot = authority.capture_snapshot().unwrap();
        client.apply_resync_snapshot(&snapshot).unwrap();
        let before_hash = client.world().state_hash().unwrap();
        let stale = relay(CommittedInputRecord {
            frame: frame(1, 0, 99),
            fighter: FighterId::ZERO,
            source: CommittedInputSource::Peer(PeerId::new(9).unwrap()),
        });

        assert_eq!(
            client.observe_committed_relay(&stale).unwrap(),
            ClientAuthorityOutcome::Ignored
        );
        assert_eq!(client.predicted_tick(), Some(SimTick(3)));
        assert_eq!(client.confirmed_tick(), Some(SimTick(3)));
        assert_eq!(client.world().state_hash().unwrap(), before_hash);
        assert_eq!(client.metrics().hard_resync_requests, 0);
        assert_eq!(client.last_hard_resync_reason(), None);
    }

    #[test]
    fn redundant_relay_window_may_straddle_a_full_resync_boundary() {
        let mut client = initialized_client();
        let mut authority = CounterWorld::initial();
        for tick in 1..=3 {
            authority
                .step(SimTick(tick), &known_frames(tick).map(Option::unwrap))
                .unwrap();
        }
        let snapshot = authority.capture_snapshot().unwrap();
        client.apply_resync_snapshot(&snapshot).unwrap();
        client.predict_next(known_frames(4)).unwrap();
        let windows = std::array::from_fn::<_, MAX_SEATS, _>(|seat| {
            let records = std::array::from_fn::<_, 4, _>(|offset| {
                let tick = 4 - offset as u64;
                CommittedInputRecord {
                    frame: frame(tick, seat, seat as i8),
                    fighter: FighterId::new(seat as u8).unwrap(),
                    source: CommittedInputSource::Peer(PeerId::new(9).unwrap()),
                }
            });
            CommittedSeatInputWindow::from_newest_first(&records).unwrap()
        });
        let overlapping = CommittedInputRelay::new(match_id(), SimTick(4), &windows).unwrap();

        assert_eq!(
            client.observe_committed_relay(&overlapping).unwrap(),
            ClientAuthorityOutcome::Ignored
        );
        assert_eq!(client.predicted_tick(), Some(SimTick(4)));
        assert_eq!(client.confirmed_tick(), Some(SimTick(3)));
        assert_eq!(client.metrics().hard_resync_requests, 0);
        assert_eq!(client.last_hard_resync_reason(), None);
    }

    #[test]
    fn committed_input_older_than_rollback_limit_requests_hard_resync() {
        let mut client = initialized_client();
        for tick in 1..=14 {
            client.predict_next(known_frames(tick)).unwrap();
        }
        let outcome = client
            .observe_committed_relay(&relay(CommittedInputRecord {
                frame: frame(1, 0, 50),
                fighter: FighterId::ZERO,
                source: CommittedInputSource::Peer(PeerId::new(9).unwrap()),
            }))
            .unwrap();
        assert!(matches!(
            outcome,
            ClientAuthorityOutcome::HardResyncRequired {
                reason: ResyncReason::HashMismatch,
                last_confirmed_tick: SimTick::ZERO,
                ..
            }
        ));
        assert_eq!(client.metrics().hard_resync_requests, 1);
    }

    #[test]
    fn authoritative_delta_corrects_and_matches_final_hash() {
        use crate::state_sync::AuthoritySnapshotHistory;

        let initial = CounterWorld::initial();
        let initial_snapshot = initial.capture_snapshot().unwrap();
        let mut client = initialized_client();
        let exact = known_frames(1).map(Option::unwrap);
        client.predict_next(exact.map(Some)).unwrap();
        client.observe_committed_relay(&full_relay(exact)).unwrap();

        let mut authority = initial.clone();
        authority.step(SimTick(1), &exact).unwrap();
        authority.snapshot.stats.damage_by_fighter[2] += 60;
        let authoritative_snapshot = authority.capture_snapshot().unwrap();
        let mut history = AuthoritySnapshotHistory::new(match_id(), 32).unwrap();
        let base = history.record_snapshot(&initial_snapshot).unwrap();
        history.record_snapshot(&authoritative_snapshot).unwrap();
        let delta = match history.build_latest_delta(base, &[]).unwrap() {
            crate::state_sync::AuthorityDeltaOutcome::Delta(message) => message,
            other => panic!("expected delta, got {other:?}"),
        };

        assert!(matches!(
            client.observe_authority_delta(&delta).unwrap(),
            ClientAuthorityOutcome::Corrected {
                authoritative_tick: SimTick(1),
                resimulated_through: SimTick(1),
            }
        ));
        assert_eq!(
            client.world().state_hash().unwrap(),
            authority.state_hash().unwrap()
        );
        assert_eq!(client.state_baseline_ack().unwrap().tick, SimTick(1));
    }

    #[test]
    fn delta_using_an_evicted_older_baseline_is_ignored() {
        use crate::state_sync::{AuthorityDeltaOutcome, AuthoritySnapshotHistory};

        let initial = CounterWorld::initial();
        let initial_snapshot = initial.capture_snapshot().unwrap();
        let mut client = PredictedClient::with_hooks(
            CounterWorld::initial(),
            match_id(),
            32,
            DiscardLog::default(),
            NoopRollbackTiming,
        )
        .unwrap();
        client.apply_initial_snapshot(&initial_snapshot).unwrap();

        let mut authority = initial;
        let mut history = AuthoritySnapshotHistory::new(match_id(), 64).unwrap();
        let initial_baseline = history.record_snapshot(&initial_snapshot).unwrap();
        let mut client_baseline = initial_baseline;

        for tick in 1..=32 {
            let inputs = known_frames(tick).map(Option::unwrap);
            client.predict_next(inputs.map(Some)).unwrap();
            client.observe_committed_relay(&full_relay(inputs)).unwrap();
            authority.step(SimTick(tick), &inputs).unwrap();
            history
                .record_snapshot(&authority.capture_snapshot().unwrap())
                .unwrap();
            let delta = match history.build_latest_delta(client_baseline, &[]).unwrap() {
                AuthorityDeltaOutcome::Delta(message) => message,
                other => panic!("expected delta, got {other:?}"),
            };
            assert!(matches!(
                client.observe_authority_delta(&delta).unwrap(),
                ClientAuthorityOutcome::Matched { tick: matched } if matched == SimTick(tick)
            ));
            client_baseline = client.acknowledged_baseline().unwrap();
        }
        assert!(!client.baselines.contains(initial_baseline));

        let inputs = known_frames(33).map(Option::unwrap);
        client.predict_next(inputs.map(Some)).unwrap();
        client.observe_committed_relay(&full_relay(inputs)).unwrap();
        authority.step(SimTick(33), &inputs).unwrap();
        history
            .record_snapshot(&authority.capture_snapshot().unwrap())
            .unwrap();
        let stale_base_delta = match history.build_latest_delta(initial_baseline, &[]).unwrap() {
            AuthorityDeltaOutcome::Delta(message) => message,
            other => panic!("expected stale-base delta, got {other:?}"),
        };

        assert_eq!(
            client.observe_authority_delta(&stale_base_delta).unwrap(),
            ClientAuthorityOutcome::Ignored
        );
        assert_eq!(client.confirmed_tick(), Some(SimTick(32)));
        assert_eq!(client.metrics().hard_resync_requests, 0);
        assert_eq!(client.metrics().deltas_ignored, 1);
    }

    #[test]
    fn reliable_snapshot_overtaken_by_confirmed_state_advances_only_delta_baseline() {
        let mut client = initialized_client();
        let mut authority = CounterWorld::initial();
        let mut first_snapshot = None;

        for tick in 1..=2 {
            let inputs = known_frames(tick).map(Option::unwrap);
            client.predict_next(inputs.map(Some)).unwrap();
            client.observe_committed_relay(&full_relay(inputs)).unwrap();
            authority.step(SimTick(tick), &inputs).unwrap();
            let snapshot = authority.capture_snapshot().unwrap();
            first_snapshot.get_or_insert_with(|| snapshot.clone());
            let state = StateHashAndAcks::new(
                match_id(),
                SimTick(tick),
                StateHash(snapshot.canonical_hash().unwrap()),
                &[],
            )
            .unwrap();
            assert!(matches!(
                client.observe_authority_hash(&state).unwrap(),
                ClientAuthorityOutcome::Matched { tick: matched } if matched == SimTick(tick)
            ));
        }

        let before_hash = client.world().state_hash().unwrap();
        let snapshot = first_snapshot.unwrap();
        let applied = client.apply_resync_snapshot(&snapshot).unwrap();
        assert_eq!(applied.tick, SimTick(1));
        assert_eq!(client.predicted_tick(), Some(SimTick(2)));
        assert_eq!(client.confirmed_tick(), Some(SimTick(2)));
        assert_eq!(client.world().state_hash().unwrap(), before_hash);
        assert_eq!(
            client.acknowledged_baseline(),
            Some(StateBaseline::new(SimTick(1), applied.hash))
        );
        assert_eq!(client.metrics().stale_resync_baselines_accepted, 1);
        assert_eq!(client.metrics().hard_resyncs_applied, 0);
    }

    #[test]
    fn state_before_committed_inputs_never_confirms_a_reordered_prediction() {
        let mut client = initialized_client();
        client.events_mut().0.clear();
        let mut predicted = known_frames(1);
        predicted[1] = None;
        client.predict_next(predicted).unwrap();

        let mut exact = known_frames(1).map(Option::unwrap);
        exact[1] = frame(1, 1, 45);
        let mut authority = CounterWorld::initial();
        authority.step(SimTick(1), &exact).unwrap();
        let state = StateHashAndAcks::new(
            match_id(),
            SimTick(1),
            StateHash(authority.state_hash().unwrap()),
            &[],
        )
        .unwrap();

        assert_eq!(
            client.observe_authority_hash(&state).unwrap(),
            ClientAuthorityOutcome::AwaitingCommittedInputs { tick: SimTick(1) }
        );
        assert_eq!(client.confirmed_tick(), Some(SimTick::ZERO));
        let outcome = client.observe_committed_relay(&full_relay(exact)).unwrap();
        assert_eq!(
            outcome,
            ClientAuthorityOutcome::Matched { tick: SimTick(1) }
        );
        assert_eq!(client.confirmed_tick(), Some(SimTick(1)));
        assert_eq!(client.metrics().hard_resync_requests, 0);
        assert_eq!(client.metrics().committed_relay_corrections, 1);
        assert_eq!(client.events().0, vec![SimTick::ZERO]);
    }

    #[test]
    fn newer_pending_hash_supersedes_an_older_pending_delta() {
        let mut client = initialized_client();
        for tick in 1..=2 {
            client.predict_next(known_frames(tick)).unwrap();
        }
        let older_delta = exact_delta_through(1);
        let newest_hash = StateHashAndAcks::new(
            match_id(),
            SimTick(2),
            StateHash(client.world().state_hash().unwrap()),
            &[],
        )
        .unwrap();

        assert_eq!(
            client.observe_authority_delta(&older_delta).unwrap(),
            ClientAuthorityOutcome::AwaitingCommittedInputs { tick: SimTick(1) }
        );
        assert_eq!(
            client.observe_authority_hash(&newest_hash).unwrap(),
            ClientAuthorityOutcome::AwaitingCommittedInputs { tick: SimTick(2) }
        );
        assert!(matches!(
            client.pending_state,
            Some(PendingAuthorityState::Hash(message))
                if message.authority_tick == SimTick(2)
        ));

        assert_eq!(
            client
                .observe_committed_relay(&full_relay(known_frames(1).map(Option::unwrap)))
                .unwrap(),
            ClientAuthorityOutcome::Ignored
        );
        assert_eq!(
            client
                .observe_committed_relay(&full_relay(known_frames(2).map(Option::unwrap)))
                .unwrap(),
            ClientAuthorityOutcome::Matched { tick: SimTick(2) }
        );
        assert_eq!(client.confirmed_tick(), Some(SimTick(2)));
        assert_eq!(client.metrics().deltas_applied, 0);
    }

    #[test]
    fn equal_tick_delta_is_preferred_over_hash_in_either_arrival_order() {
        for delta_first in [false, true] {
            let mut client = initialized_client();
            client.predict_next(known_frames(1)).unwrap();
            let delta = exact_delta_through(1);
            let hash =
                StateHashAndAcks::new(match_id(), SimTick(1), delta.state_hash, &[]).unwrap();

            if delta_first {
                client.observe_authority_delta(&delta).unwrap();
                client.observe_authority_hash(&hash).unwrap();
            } else {
                client.observe_authority_hash(&hash).unwrap();
                client.observe_authority_delta(&delta).unwrap();
            }
            assert!(matches!(
                client.pending_state,
                Some(PendingAuthorityState::Delta(message))
                    if message.authority_tick == SimTick(1)
            ));
            assert_eq!(
                client
                    .observe_committed_relay(&full_relay(known_frames(1).map(Option::unwrap)))
                    .unwrap(),
                ClientAuthorityOutcome::Matched { tick: SimTick(1) }
            );
            assert_eq!(client.metrics().deltas_applied, 1);
        }
    }

    #[test]
    fn equal_tick_conflicting_authority_hashes_fail_closed() {
        let mut client = initialized_client();
        client.predict_next(known_frames(1)).unwrap();
        let retained = StateHash(11);
        let offered = StateHash(12);
        let first = StateHashAndAcks::new(match_id(), SimTick(1), retained, &[]).unwrap();
        let conflict = StateHashAndAcks::new(match_id(), SimTick(1), offered, &[]).unwrap();

        client.observe_authority_hash(&first).unwrap();
        assert!(matches!(
            client.observe_authority_hash(&conflict),
            Err(PredictedClientError::ConflictingAuthorityState {
                tick: SimTick(1),
                retained: actual_retained,
                offered: actual_offered,
            }) if actual_retained == retained && actual_offered == offered
        ));
    }

    #[test]
    fn hard_resync_rebases_newer_committed_coverage_and_pending_state() {
        let mut client = initialized_client();
        let inputs = known_frames(1).map(Option::unwrap);
        let authority = authority_through(1);
        let state = StateHashAndAcks::new(
            match_id(),
            SimTick(1),
            StateHash(authority.state_hash().unwrap()),
            &[],
        )
        .unwrap();

        assert_eq!(
            client.observe_committed_relay(&full_relay(inputs)).unwrap(),
            ClientAuthorityOutcome::Ignored
        );
        assert_eq!(
            client.observe_authority_hash(&state).unwrap(),
            ClientAuthorityOutcome::AwaitingCommittedInputs { tick: SimTick(1) }
        );
        assert_eq!(client.latest_relay_tick, Some(SimTick(1)));

        let boundary = CounterWorld::initial().capture_snapshot().unwrap();
        client.apply_resync_snapshot(&boundary).unwrap();
        assert!(matches!(
            client.pending_state,
            Some(PendingAuthorityState::Hash(message))
                if message.authority_tick == SimTick(1)
        ));
        assert_eq!(client.latest_relay_tick, Some(SimTick(1)));
        assert_eq!(client.relay_recoverable_oldest, Some(SimTick(1)));

        client.predict_next([None; MAX_SEATS]).unwrap();
        assert_eq!(
            client.process_pending_authority().unwrap(),
            ClientAuthorityOutcome::Matched { tick: SimTick(1) }
        );
        assert_eq!(client.confirmed_tick(), Some(SimTick(1)));
        let replayed = client
            .prediction()
            .unwrap()
            .inputs_at(SimTick(1))
            .expect("rebased committed input is retained");
        for (seat, expected) in inputs.into_iter().enumerate() {
            assert_eq!(*replayed.frame(SeatId::new(seat as u8).unwrap()), expected);
        }
    }

    #[test]
    fn stale_snapshot_prunes_only_confirmed_boundary_and_keeps_newer_pending_state() {
        let mut client = initialized_client();
        let mut tick_one_snapshot = None;
        for tick in 1..=2 {
            let inputs = known_frames(tick).map(Option::unwrap);
            client.predict_next(inputs.map(Some)).unwrap();
            client.observe_committed_relay(&full_relay(inputs)).unwrap();
            let authority = authority_through(tick);
            if tick == 1 {
                tick_one_snapshot = Some(authority.capture_snapshot().unwrap());
            }
            let state = StateHashAndAcks::new(
                match_id(),
                SimTick(tick),
                StateHash(authority.state_hash().unwrap()),
                &[],
            )
            .unwrap();
            client.observe_authority_hash(&state).unwrap();
        }
        client.predict_next(known_frames(3)).unwrap();
        let tick_three = authority_through(3);
        let final_state = StateHashAndAcks::new(
            match_id(),
            SimTick(3),
            StateHash(tick_three.state_hash().unwrap()),
            &[],
        )
        .unwrap();
        assert_eq!(
            client.observe_authority_hash(&final_state).unwrap(),
            ClientAuthorityOutcome::AwaitingCommittedInputs { tick: SimTick(3) }
        );

        client
            .apply_resync_snapshot(&tick_one_snapshot.unwrap())
            .unwrap();
        assert_eq!(client.confirmed_tick(), Some(SimTick(2)));
        assert!(matches!(
            client.pending_state,
            Some(PendingAuthorityState::Hash(message))
                if message.authority_tick == SimTick(3)
        ));
        assert_eq!(
            client
                .observe_committed_relay(&full_relay(known_frames(3).map(Option::unwrap)))
                .unwrap(),
            ClientAuthorityOutcome::Matched { tick: SimTick(3) }
        );
        assert_eq!(client.confirmed_tick(), Some(SimTick(3)));
    }

    #[test]
    fn relay_loss_beyond_redundancy_window_requests_history_resync() {
        let mut client = initialized_client();
        for tick in 1..=9 {
            client.predict_next(known_frames(tick)).unwrap();
        }
        let state = StateHashAndAcks::new(
            match_id(),
            SimTick(9),
            StateHash(client.world().state_hash().unwrap()),
            &[],
        )
        .unwrap();
        assert_eq!(
            client.observe_authority_hash(&state).unwrap(),
            ClientAuthorityOutcome::AwaitingCommittedInputs { tick: SimTick(9) }
        );

        assert!(matches!(
            client
                .observe_committed_relay(&redundant_full_relay(9))
                .unwrap(),
            ClientAuthorityOutcome::HardResyncRequired {
                reason: ResyncReason::HistoryExpired,
                last_confirmed_tick: SimTick::ZERO,
                ..
            }
        ));
        assert_eq!(client.metrics().hard_resync_requests, 1);
    }
}
