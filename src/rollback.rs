//! Bounded full-world client prediction and rollback.
//!
//! The engine owns only prediction bookkeeping: four-seat tick inputs, predicted
//! snapshots and hashes, confirmation state, and non-canonical metrics. Gameplay
//! remains behind [`RollbackWorld`], so the same driver can operate an ECS-backed
//! world or a small deterministic fixture. Presentation events are not stored in
//! snapshots; callers discard them through [`RollbackEventDiscard`] before any
//! restored tick is simulated again.

use crate::network_protocol::{
    InputButtons, InputFrame, MAX_SEATS, ProtocolValidationError, SeatId,
};
use crate::simulation::SimTick;
use crate::snapshot::{
    CanonicalSnapshot, MAX_SNAPSHOT_HISTORY, MIN_SNAPSHOT_HISTORY, SnapshotError,
};
use std::fmt;
use std::time::{Duration, Instant};

pub const NORMAL_ROLLBACK_LIMIT_TICKS: u64 = 12;
pub const DEFAULT_ROLLBACK_HISTORY_TICKS: usize = 64;

/// Snapshot-side operations required by the generic rollback engine.
pub trait RollbackSnapshot: Clone {
    type Error;

    fn rollback_tick(&self) -> SimTick;
    fn rollback_hash(&self) -> Result<u64, Self::Error>;
}

/// The production canonical schema is directly usable by any eventual ECS world
/// adapter implementing [`RollbackWorld<Snapshot = CanonicalSnapshot>`].
impl RollbackSnapshot for CanonicalSnapshot {
    type Error = SnapshotError;

    fn rollback_tick(&self) -> SimTick {
        self.header.tick
    }

    fn rollback_hash(&self) -> Result<u64, Self::Error> {
        self.canonical_hash()
    }
}

/// Minimal deterministic-world boundary. Rendering, wall clocks, sockets, and
/// presentation entities must stay outside this interface.
pub trait RollbackWorld {
    type Snapshot: RollbackSnapshot;
    type Error;

    fn current_tick(&self) -> SimTick;
    fn capture_snapshot(&self) -> Result<Self::Snapshot, Self::Error>;
    fn capture_snapshot_reusing(
        &self,
        reusable: Option<Self::Snapshot>,
    ) -> Result<Self::Snapshot, Self::Error> {
        drop(reusable);
        self.capture_snapshot()
    }
    fn restore_snapshot(&mut self, snapshot: &Self::Snapshot) -> Result<(), Self::Error>;
    fn step(&mut self, tick: SimTick, inputs: &[InputFrame; MAX_SEATS]) -> Result<(), Self::Error>;
    fn state_hash(&self) -> Result<u64, Self::Error>;
}

/// Called exactly once before snapshots/events newer than a restored tick are
/// discarded. An implementation typically truncates predicted `SimEventId`s and
/// presentation deduplication state after `retained_through`.
pub trait RollbackEventDiscard {
    fn discard_after(&mut self, retained_through: SimTick);
}

impl<F> RollbackEventDiscard for F
where
    F: FnMut(SimTick),
{
    fn discard_after(&mut self, retained_through: SimTick) {
        self(retained_through);
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoopEventDiscard;

impl RollbackEventDiscard for NoopEventDiscard {
    fn discard_after(&mut self, _retained_through: SimTick) {}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RollbackOperation {
    AuthoritativeCorrection,
    LateInputCorrection,
    HardResync,
}

/// Non-canonical cost measurement. Hooks may read a wall clock because their
/// output is recorded only in diagnostics and never fed back into simulation.
pub trait RollbackTimingHook {
    fn begin(&mut self, _operation: RollbackOperation, _depth_ticks: u64) {}

    fn finish(&mut self, _operation: RollbackOperation, _depth_ticks: u64) -> Duration {
        Duration::ZERO
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoopRollbackTiming;

impl RollbackTimingHook for NoopRollbackTiming {}

#[derive(Debug, Default)]
pub struct InstantRollbackTiming {
    started: Option<Instant>,
}

impl RollbackTimingHook for InstantRollbackTiming {
    fn begin(&mut self, _operation: RollbackOperation, _depth_ticks: u64) {
        self.started = Some(Instant::now());
    }

    fn finish(&mut self, _operation: RollbackOperation, _depth_ticks: u64) -> Duration {
        self.started
            .take()
            .map_or(Duration::ZERO, |start| start.elapsed())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RollbackMetrics {
    pub predicted_ticks: u64,
    pub predicted_input_frames: u64,
    pub missing_input_predictions: u64,
    pub authoritative_hash_comparisons: u64,
    pub authoritative_hash_matches: u64,
    pub authoritative_hash_mismatches: u64,
    pub stale_authoritative_updates: u64,
    pub corrections: u64,
    pub late_input_corrections: u64,
    pub hard_resyncs_required: u64,
    pub hard_resyncs_applied: u64,
    pub resimulated_ticks: u64,
    /// Largest applied prediction correction. Hard-resync discard depth is kept
    /// separately so acceptance gates can prove normal rollback never crossed
    /// the configured cap.
    pub maximum_normal_rollback_depth: u64,
    pub maximum_hard_resync_discard_depth: u64,
    /// Largest operation of either kind, retained for backwards-compatible
    /// dashboards that graph one combined correction-depth series.
    pub maximum_rollback_depth: u64,
    pub total_correction_nanoseconds: u128,
    pub maximum_correction_nanoseconds: u128,
    pub snapshot_history_high_water: usize,
    pub input_history_high_water: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CanonicalTickInputs {
    pub tick: SimTick,
    frames: [InputFrame; MAX_SEATS],
    known_mask: u8,
}

impl CanonicalTickInputs {
    pub fn from_frames(
        tick: SimTick,
        frames: [InputFrame; MAX_SEATS],
        known_mask: u8,
    ) -> Result<Self, InputSetError> {
        if known_mask & !((1_u8 << MAX_SEATS) - 1) != 0 {
            return Err(InputSetError::InvalidKnownMask(known_mask));
        }
        for (seat_index, frame) in frames.iter().enumerate() {
            frame.validate().map_err(InputSetError::InvalidFrame)?;
            if frame.tick != tick {
                return Err(InputSetError::WrongTick {
                    expected: tick,
                    found: frame.tick,
                });
            }
            if usize::from(frame.seat.get()) != seat_index {
                return Err(InputSetError::WrongSeat {
                    expected: seat_index as u8,
                    found: frame.seat.get(),
                });
            }
        }
        Ok(Self {
            tick,
            frames,
            known_mask,
        })
    }

    pub const fn frames(&self) -> &[InputFrame; MAX_SEATS] {
        &self.frames
    }

    pub const fn known_mask(&self) -> u8 {
        self.known_mask
    }

    pub fn is_known(&self, seat: SeatId) -> bool {
        self.known_mask & (1 << seat.get()) != 0
    }

    pub fn was_predicted(&self, seat: SeatId) -> bool {
        !self.is_known(seat)
    }

    pub fn frame(&self, seat: SeatId) -> &InputFrame {
        &self.frames[usize::from(seat.get())]
    }

    fn replace_frame(&mut self, frame: InputFrame, known: bool) {
        let seat_index = usize::from(frame.seat.get());
        self.frames[seat_index] = frame;
        if known {
            self.known_mask |= 1 << seat_index;
        } else {
            self.known_mask &= !(1 << seat_index);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputSetError {
    InvalidFrame(ProtocolValidationError),
    InvalidKnownMask(u8),
    WrongTick { expected: SimTick, found: SimTick },
    WrongSeat { expected: u8, found: u8 },
}

impl fmt::Display for InputSetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid rollback input set: {self:?}")
    }
}

impl std::error::Error for InputSetError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HardResyncReason {
    AuthorityAhead {
        authority_tick: SimTick,
        predicted_tick: SimTick,
    },
    SnapshotHistoryExpired {
        requested_tick: SimTick,
        oldest_available: Option<SimTick>,
    },
    MissingPredictedSnapshot {
        tick: SimTick,
    },
    MissingInputHistory {
        tick: SimTick,
    },
    MissingAuthoritativeSnapshot {
        tick: SimTick,
    },
    RollbackDepthExceeded {
        depth_ticks: u64,
        maximum_ticks: u64,
    },
    ConfirmedStateContradiction {
        tick: SimTick,
    },
    ConfirmedInputChanged {
        tick: SimTick,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReconcileOutcome {
    Matched {
        tick: SimTick,
        hash: u64,
        confirmed_advanced: bool,
    },
    Corrected {
        authoritative_tick: SimTick,
        resimulated_through: SimTick,
        depth_ticks: u64,
    },
    StaleAuthority {
        received_tick: SimTick,
        confirmed_tick: SimTick,
    },
    HardResyncRequired(HardResyncReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LateInputOutcome {
    Unchanged {
        tick: SimTick,
        newly_known: bool,
    },
    Corrected {
        input_tick: SimTick,
        restored_tick: SimTick,
        resimulated_through: SimTick,
        depth_ticks: u64,
    },
    HardResyncRequired(HardResyncReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HardResyncOutcome {
    pub authoritative_tick: SimTick,
    pub discarded_predicted_ticks: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RollbackError<WorldError, SnapshotHashError> {
    World(WorldError),
    SnapshotHash(SnapshotHashError),
    InvalidHistoryCapacity {
        requested: usize,
        minimum: usize,
        maximum: usize,
    },
    Input(InputSetError),
    TimelineExhausted,
    WorldTickMismatch {
        expected: SimTick,
        found: SimTick,
    },
    SnapshotTickMismatch {
        expected: SimTick,
        found: SimTick,
    },
    AuthoritativeSnapshotHashMismatch {
        declared: u64,
        snapshot: u64,
    },
    RestoredWorldHashMismatch {
        expected: u64,
        found: u64,
    },
    LateInputIsFuture {
        input_tick: SimTick,
        predicted_tick: SimTick,
    },
    LateInputAtInitialTick,
    ConfirmedTickRegression {
        current: SimTick,
        requested: SimTick,
    },
}

impl<WorldError: fmt::Debug, SnapshotHashError: fmt::Debug> fmt::Display
    for RollbackError<WorldError, SnapshotHashError>
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "rollback operation failed: {self:?}")
    }
}

impl<WorldError, SnapshotHashError> std::error::Error
    for RollbackError<WorldError, SnapshotHashError>
where
    WorldError: std::error::Error + 'static,
    SnapshotHashError: std::error::Error + 'static,
{
}

#[derive(Clone)]
struct PredictedSnapshot<S> {
    snapshot: S,
    hash: u64,
}

#[derive(Clone)]
struct TickEntry<T> {
    tick: SimTick,
    value: T,
}

#[derive(Clone)]
struct BoundedTickRing<T> {
    slots: Vec<Option<TickEntry<T>>>,
    len: usize,
}

impl<T> BoundedTickRing<T> {
    fn new(capacity: usize) -> Self {
        Self {
            slots: (0..capacity).map(|_| None).collect(),
            len: 0,
        }
    }

    fn capacity(&self) -> usize {
        self.slots.len()
    }

    fn len(&self) -> usize {
        self.len
    }

    fn slot(&self, tick: SimTick) -> usize {
        (tick.0 % self.capacity() as u64) as usize
    }

    fn get(&self, tick: SimTick) -> Option<&T> {
        self.slots[self.slot(tick)]
            .as_ref()
            .filter(|entry| entry.tick == tick)
            .map(|entry| &entry.value)
    }

    fn insert(&mut self, tick: SimTick, value: T) {
        drop(self.insert_replacing(tick, value));
    }

    fn insert_replacing(&mut self, tick: SimTick, value: T) -> Option<T> {
        let slot = self.slot(tick);
        if self.slots[slot].is_none() {
            self.len += 1;
        }
        self.slots[slot]
            .replace(TickEntry { tick, value })
            .map(|entry| entry.value)
    }

    fn take(&mut self, tick: SimTick) -> Option<T> {
        let slot = self.slot(tick);
        if !self.slots[slot]
            .as_ref()
            .is_some_and(|entry| entry.tick == tick)
        {
            return None;
        }
        self.len -= 1;
        self.slots[slot].take().map(|entry| entry.value)
    }

    fn remove_after_with(&mut self, tick: SimTick, mut remove: impl FnMut(T)) -> usize {
        let mut removed = 0;
        for slot in &mut self.slots {
            if slot.as_ref().is_some_and(|entry| entry.tick > tick) {
                let entry = slot
                    .take()
                    .expect("a tick-newer bounded-ring entry was present");
                remove(entry.value);
                removed += 1;
            }
        }
        self.len -= removed;
        removed
    }

    fn oldest_tick(&self) -> Option<SimTick> {
        self.slots
            .iter()
            .filter_map(|slot| slot.as_ref().map(|entry| entry.tick))
            .min()
    }

    fn clear(&mut self) {
        for slot in &mut self.slots {
            *slot = None;
        }
        self.len = 0;
    }
}

pub struct PredictionEngine<S: RollbackSnapshot> {
    predicted_tick: SimTick,
    confirmed_tick: SimTick,
    last_inputs: [InputFrame; MAX_SEATS],
    inputs: BoundedTickRing<CanonicalTickInputs>,
    snapshots: BoundedTickRing<PredictedSnapshot<S>>,
    recycled_snapshots: Vec<S>,
    metrics: RollbackMetrics,
}

impl<S: RollbackSnapshot> PredictionEngine<S> {
    pub fn new<W>(
        world: &W,
        history_capacity: usize,
    ) -> Result<Self, RollbackError<W::Error, S::Error>>
    where
        W: RollbackWorld<Snapshot = S>,
    {
        if !(MIN_SNAPSHOT_HISTORY..=MAX_SNAPSHOT_HISTORY).contains(&history_capacity) {
            return Err(RollbackError::InvalidHistoryCapacity {
                requested: history_capacity,
                minimum: MIN_SNAPSHOT_HISTORY,
                maximum: MAX_SNAPSHOT_HISTORY,
            });
        }
        let tick = world.current_tick();
        let snapshot = world.capture_snapshot().map_err(RollbackError::World)?;
        ensure_snapshot_tick::<W::Error, S::Error>(&snapshot, tick)?;
        let hash = snapshot
            .rollback_hash()
            .map_err(RollbackError::SnapshotHash)?;
        let mut snapshots = BoundedTickRing::new(history_capacity);
        snapshots.insert(tick, PredictedSnapshot { snapshot, hash });
        let last_inputs = std::array::from_fn(|seat| neutral_frame(tick, seat));

        Ok(Self {
            predicted_tick: tick,
            confirmed_tick: tick,
            last_inputs,
            inputs: BoundedTickRing::new(history_capacity),
            snapshots,
            recycled_snapshots: Vec::with_capacity(history_capacity),
            metrics: RollbackMetrics {
                snapshot_history_high_water: 1,
                ..RollbackMetrics::default()
            },
        })
    }

    pub const fn predicted_tick(&self) -> SimTick {
        self.predicted_tick
    }

    pub const fn confirmed_tick(&self) -> SimTick {
        self.confirmed_tick
    }

    pub const fn metrics(&self) -> RollbackMetrics {
        self.metrics
    }

    pub fn history_capacity(&self) -> usize {
        self.snapshots.capacity()
    }

    pub fn snapshot_history_len(&self) -> usize {
        self.snapshots.len()
    }

    pub fn input_history_len(&self) -> usize {
        self.inputs.len()
    }

    pub fn oldest_snapshot_tick(&self) -> Option<SimTick> {
        self.snapshots.oldest_tick()
    }

    pub fn predicted_hash(&self, tick: SimTick) -> Option<u64> {
        self.snapshots.get(tick).map(|entry| entry.hash)
    }

    /// Returns the immutable retained canonical snapshot for an exact tick.
    /// Callers use this for result projection; it never restores or mutates the
    /// prediction timeline.
    pub fn snapshot_at(&self, tick: SimTick) -> Option<&S> {
        self.snapshots.get(tick).map(|entry| &entry.snapshot)
    }

    pub fn inputs_at(&self, tick: SimTick) -> Option<&CanonicalTickInputs> {
        self.inputs.get(tick)
    }

    /// Installs the exact continuous input state at the current confirmed
    /// snapshot boundary. This is used only after a reliable resync has replaced
    /// the world. Older tail records are validation/audit evidence and are not
    /// replayed: rollback before the new confirmed boundary is forbidden.
    pub fn seed_input_boundary(
        &mut self,
        frames: [InputFrame; MAX_SEATS],
    ) -> Result<(), InputSetError> {
        for (seat_index, frame) in frames.iter().copied().enumerate() {
            validate_frame_for(frame, self.confirmed_tick, seat_index)?;
        }
        self.last_inputs = frames;
        Ok(())
    }

    pub const fn input_boundary(&self) -> &[InputFrame; MAX_SEATS] {
        &self.last_inputs
    }

    /// Advances one sequential predicted tick. A `None` seat repeats movement and
    /// held buttons from its previous prediction while clearing both edge masks.
    pub fn predict_next<W>(
        &mut self,
        world: &mut W,
        provided: [Option<InputFrame>; MAX_SEATS],
    ) -> Result<SimTick, RollbackError<W::Error, S::Error>>
    where
        W: RollbackWorld<Snapshot = S>,
    {
        let next_tick = self
            .predicted_tick
            .0
            .checked_add(1)
            .map(SimTick)
            .ok_or(RollbackError::TimelineExhausted)?;
        let mut frames = self.last_inputs;
        let mut known_mask = 0_u8;

        for seat_index in 0..MAX_SEATS {
            frames[seat_index] = match provided[seat_index] {
                Some(frame) => {
                    validate_frame_for(frame, next_tick, seat_index)
                        .map_err(RollbackError::Input)?;
                    known_mask |= 1 << seat_index;
                    frame
                }
                None => {
                    self.metrics.missing_input_predictions =
                        self.metrics.missing_input_predictions.saturating_add(1);
                    repeated_frame(self.last_inputs[seat_index], next_tick, seat_index)
                }
            };
        }
        let tick_inputs = CanonicalTickInputs::from_frames(next_tick, frames, known_mask)
            .map_err(RollbackError::Input)?;

        world
            .step(next_tick, tick_inputs.frames())
            .map_err(RollbackError::World)?;
        ensure_world_tick::<W, S>(world, next_tick)?;
        let reusable = self.recycled_snapshots.pop();
        let predicted = capture_predicted::<W, S>(world, next_tick, reusable)?;

        self.inputs.insert(next_tick, tick_inputs);
        self.insert_predicted_snapshot(next_tick, predicted);
        self.last_inputs = frames;
        self.predicted_tick = next_tick;
        self.metrics.predicted_ticks = self.metrics.predicted_ticks.saturating_add(1);
        self.metrics.predicted_input_frames = self
            .metrics
            .predicted_input_frames
            .saturating_add(MAX_SEATS as u64);
        self.update_history_high_water();
        Ok(next_tick)
    }

    /// Compares one authoritative hash. A mismatch is corrected only when the
    /// matching authoritative full snapshot is supplied and the entire replay
    /// interval remains inside both the 12-tick cap and bounded input history.
    pub fn reconcile<W>(
        &mut self,
        world: &mut W,
        authoritative_tick: SimTick,
        authoritative_hash: u64,
        authoritative_snapshot: Option<&S>,
        events: &mut impl RollbackEventDiscard,
        timing: &mut impl RollbackTimingHook,
    ) -> Result<ReconcileOutcome, RollbackError<W::Error, S::Error>>
    where
        W: RollbackWorld<Snapshot = S>,
    {
        if authoritative_tick < self.confirmed_tick {
            self.metrics.stale_authoritative_updates =
                self.metrics.stale_authoritative_updates.saturating_add(1);
            return Ok(ReconcileOutcome::StaleAuthority {
                received_tick: authoritative_tick,
                confirmed_tick: self.confirmed_tick,
            });
        }
        if authoritative_tick > self.predicted_tick {
            return Ok(self.require_hard_resync(HardResyncReason::AuthorityAhead {
                authority_tick: authoritative_tick,
                predicted_tick: self.predicted_tick,
            }));
        }

        self.metrics.authoritative_hash_comparisons = self
            .metrics
            .authoritative_hash_comparisons
            .saturating_add(1);
        let Some(predicted) = self.snapshots.get(authoritative_tick) else {
            let reason = if self
                .snapshots
                .oldest_tick()
                .is_some_and(|oldest| authoritative_tick < oldest)
            {
                HardResyncReason::SnapshotHistoryExpired {
                    requested_tick: authoritative_tick,
                    oldest_available: self.snapshots.oldest_tick(),
                }
            } else {
                HardResyncReason::MissingPredictedSnapshot {
                    tick: authoritative_tick,
                }
            };
            return Ok(self.require_hard_resync(reason));
        };
        let predicted_hash = predicted.hash;
        if predicted_hash == authoritative_hash {
            self.metrics.authoritative_hash_matches =
                self.metrics.authoritative_hash_matches.saturating_add(1);
            let advanced = authoritative_tick > self.confirmed_tick;
            self.confirmed_tick = self.confirmed_tick.max(authoritative_tick);
            return Ok(ReconcileOutcome::Matched {
                tick: authoritative_tick,
                hash: authoritative_hash,
                confirmed_advanced: advanced,
            });
        }

        self.metrics.authoritative_hash_mismatches =
            self.metrics.authoritative_hash_mismatches.saturating_add(1);
        if authoritative_tick == self.confirmed_tick {
            return Ok(
                self.require_hard_resync(HardResyncReason::ConfirmedStateContradiction {
                    tick: authoritative_tick,
                }),
            );
        }
        let Some(authoritative_snapshot) = authoritative_snapshot else {
            return Ok(
                self.require_hard_resync(HardResyncReason::MissingAuthoritativeSnapshot {
                    tick: authoritative_tick,
                }),
            );
        };
        ensure_snapshot_tick::<W::Error, S::Error>(authoritative_snapshot, authoritative_tick)?;
        let snapshot_hash = authoritative_snapshot
            .rollback_hash()
            .map_err(RollbackError::SnapshotHash)?;
        if snapshot_hash != authoritative_hash {
            return Err(RollbackError::AuthoritativeSnapshotHashMismatch {
                declared: authoritative_hash,
                snapshot: snapshot_hash,
            });
        }

        let depth = self.predicted_tick.0 - authoritative_tick.0;
        if depth > NORMAL_ROLLBACK_LIMIT_TICKS {
            return Ok(
                self.require_hard_resync(HardResyncReason::RollbackDepthExceeded {
                    depth_ticks: depth,
                    maximum_ticks: NORMAL_ROLLBACK_LIMIT_TICKS,
                }),
            );
        }
        if let Some(missing_tick) = self.first_missing_input_after(authoritative_tick) {
            return Ok(
                self.require_hard_resync(HardResyncReason::MissingInputHistory {
                    tick: missing_tick,
                }),
            );
        }

        timing.begin(RollbackOperation::AuthoritativeCorrection, depth);
        let result = self.restore_and_resimulate(
            world,
            authoritative_tick,
            authoritative_snapshot.clone(),
            authoritative_hash,
            events,
        );
        let elapsed = timing.finish(RollbackOperation::AuthoritativeCorrection, depth);
        self.record_correction_cost(RollbackOperation::AuthoritativeCorrection, depth, elapsed);
        result?;

        self.confirmed_tick = self.confirmed_tick.max(authoritative_tick);
        self.metrics.corrections = self.metrics.corrections.saturating_add(1);
        Ok(ReconcileOutcome::Corrected {
            authoritative_tick,
            resimulated_through: self.predicted_tick,
            depth_ticks: depth,
        })
    }

    /// Replaces an already-predicted frame and immediately replays from the tick
    /// before it. Later missing predictions for that seat are regenerated from the
    /// corrected continuous state until a known frame supersedes them.
    pub fn apply_late_input<W>(
        &mut self,
        world: &mut W,
        frame: InputFrame,
        events: &mut impl RollbackEventDiscard,
        timing: &mut impl RollbackTimingHook,
    ) -> Result<LateInputOutcome, RollbackError<W::Error, S::Error>>
    where
        W: RollbackWorld<Snapshot = S>,
    {
        frame
            .validate()
            .map_err(|error| RollbackError::Input(InputSetError::InvalidFrame(error)))?;
        if frame.tick > self.predicted_tick {
            return Err(RollbackError::LateInputIsFuture {
                input_tick: frame.tick,
                predicted_tick: self.predicted_tick,
            });
        }
        if frame.tick == SimTick::ZERO {
            return Err(RollbackError::LateInputAtInitialTick);
        }
        let seat_index = usize::from(frame.seat.get());
        let Some(existing) = self.inputs.get(frame.tick).copied() else {
            return Ok(LateInputOutcome::HardResyncRequired(self.note_hard_resync(
                HardResyncReason::MissingInputHistory { tick: frame.tick },
            )));
        };
        validate_frame_for(frame, frame.tick, seat_index).map_err(RollbackError::Input)?;
        let existing_frame = existing.frames[seat_index];
        if simulation_equivalent_input(existing_frame, frame) {
            let newly_known = !existing.is_known(frame.seat);
            if newly_known || existing_frame.sequence != frame.sequence {
                let mut updated = existing;
                updated.replace_frame(frame, true);
                self.inputs.insert(frame.tick, updated);
                if frame.tick == self.predicted_tick {
                    self.last_inputs[seat_index] = frame;
                }
            }
            return Ok(LateInputOutcome::Unchanged {
                tick: frame.tick,
                newly_known,
            });
        }
        if frame.tick <= self.confirmed_tick {
            return Ok(LateInputOutcome::HardResyncRequired(self.note_hard_resync(
                HardResyncReason::ConfirmedInputChanged { tick: frame.tick },
            )));
        }

        let restored_tick = SimTick(frame.tick.0 - 1);
        let depth = self.predicted_tick.0 - restored_tick.0;
        if depth > NORMAL_ROLLBACK_LIMIT_TICKS {
            return Ok(LateInputOutcome::HardResyncRequired(self.note_hard_resync(
                HardResyncReason::RollbackDepthExceeded {
                    depth_ticks: depth,
                    maximum_ticks: NORMAL_ROLLBACK_LIMIT_TICKS,
                },
            )));
        }
        let Some(_) = self.snapshots.get(restored_tick) else {
            let reason = if self
                .snapshots
                .oldest_tick()
                .is_some_and(|oldest| restored_tick < oldest)
            {
                HardResyncReason::SnapshotHistoryExpired {
                    requested_tick: restored_tick,
                    oldest_available: self.snapshots.oldest_tick(),
                }
            } else {
                HardResyncReason::MissingPredictedSnapshot {
                    tick: restored_tick,
                }
            };
            return Ok(LateInputOutcome::HardResyncRequired(
                self.note_hard_resync(reason),
            ));
        };
        if let Some(missing_tick) = self.first_missing_input_after(restored_tick) {
            return Ok(LateInputOutcome::HardResyncRequired(self.note_hard_resync(
                HardResyncReason::MissingInputHistory { tick: missing_tick },
            )));
        }

        self.replace_late_input_and_repredict(frame);
        let restored = self
            .snapshots
            .take(restored_tick)
            .expect("the restored snapshot was preflighted before rollback mutation");
        timing.begin(RollbackOperation::LateInputCorrection, depth);
        let result = self.restore_and_resimulate(
            world,
            restored_tick,
            restored.snapshot,
            restored.hash,
            events,
        );
        let elapsed = timing.finish(RollbackOperation::LateInputCorrection, depth);
        self.record_correction_cost(RollbackOperation::LateInputCorrection, depth, elapsed);
        result?;

        self.metrics.corrections = self.metrics.corrections.saturating_add(1);
        self.metrics.late_input_corrections = self.metrics.late_input_corrections.saturating_add(1);
        Ok(LateInputOutcome::Corrected {
            input_tick: frame.tick,
            restored_tick,
            resimulated_through: self.predicted_tick,
            depth_ticks: depth,
        })
    }

    /// Applies a full snapshot after a `HardResyncRequired` outcome. All predicted
    /// inputs and snapshots are discarded; the supplied snapshot becomes both the
    /// predicted and confirmed boundary.
    pub fn apply_hard_resync<W>(
        &mut self,
        world: &mut W,
        authoritative_snapshot: &S,
        authoritative_hash: u64,
        events: &mut impl RollbackEventDiscard,
        timing: &mut impl RollbackTimingHook,
    ) -> Result<HardResyncOutcome, RollbackError<W::Error, S::Error>>
    where
        W: RollbackWorld<Snapshot = S>,
    {
        let authoritative_tick = authoritative_snapshot.rollback_tick();
        if authoritative_tick < self.confirmed_tick {
            return Err(RollbackError::ConfirmedTickRegression {
                current: self.confirmed_tick,
                requested: authoritative_tick,
            });
        }
        let snapshot_hash = authoritative_snapshot
            .rollback_hash()
            .map_err(RollbackError::SnapshotHash)?;
        if snapshot_hash != authoritative_hash {
            return Err(RollbackError::AuthoritativeSnapshotHashMismatch {
                declared: authoritative_hash,
                snapshot: snapshot_hash,
            });
        }
        let discarded = self.predicted_tick.0.saturating_sub(authoritative_tick.0);

        timing.begin(RollbackOperation::HardResync, discarded);
        events.discard_after(authoritative_tick);
        let result: Result<(), RollbackError<W::Error, S::Error>> = (|| {
            world
                .restore_snapshot(authoritative_snapshot)
                .map_err(RollbackError::World)?;
            ensure_world_tick::<W, S>(world, authoritative_tick)?;
            let restored_hash = world.state_hash().map_err(RollbackError::World)?;
            if restored_hash != authoritative_hash {
                Err(RollbackError::RestoredWorldHashMismatch {
                    expected: authoritative_hash,
                    found: restored_hash,
                })
            } else {
                Ok(())
            }
        })();
        let elapsed = timing.finish(RollbackOperation::HardResync, discarded);
        self.record_correction_cost(RollbackOperation::HardResync, discarded, elapsed);
        result?;

        self.inputs.clear();
        self.snapshots.clear();
        self.snapshots.insert(
            authoritative_tick,
            PredictedSnapshot {
                snapshot: authoritative_snapshot.clone(),
                hash: authoritative_hash,
            },
        );
        self.last_inputs = std::array::from_fn(|seat| neutral_frame(authoritative_tick, seat));
        self.predicted_tick = authoritative_tick;
        self.confirmed_tick = authoritative_tick;
        self.metrics.hard_resyncs_applied = self.metrics.hard_resyncs_applied.saturating_add(1);
        self.update_history_high_water();
        Ok(HardResyncOutcome {
            authoritative_tick,
            discarded_predicted_ticks: discarded,
        })
    }

    fn restore_and_resimulate<W>(
        &mut self,
        world: &mut W,
        restored_tick: SimTick,
        restored_snapshot: S,
        restored_hash: u64,
        events: &mut impl RollbackEventDiscard,
    ) -> Result<(), RollbackError<W::Error, S::Error>>
    where
        W: RollbackWorld<Snapshot = S>,
    {
        let target_tick = self.predicted_tick;
        events.discard_after(restored_tick);
        world
            .restore_snapshot(&restored_snapshot)
            .map_err(RollbackError::World)?;
        ensure_world_tick::<W, S>(world, restored_tick)?;
        let actual_restored_hash = world.state_hash().map_err(RollbackError::World)?;
        if actual_restored_hash != restored_hash {
            return Err(RollbackError::RestoredWorldHashMismatch {
                expected: restored_hash,
                found: actual_restored_hash,
            });
        }

        {
            let recycled = &mut self.recycled_snapshots;
            let recycle_capacity = self.snapshots.capacity();
            self.snapshots.remove_after_with(restored_tick, |removed| {
                if recycled.len() < recycle_capacity {
                    recycled.push(removed.snapshot);
                }
            });
        }
        self.insert_predicted_snapshot(
            restored_tick,
            PredictedSnapshot {
                snapshot: restored_snapshot,
                hash: restored_hash,
            },
        );

        let mut tick = restored_tick.0;
        while tick < target_tick.0 {
            tick += 1;
            let tick = SimTick(tick);
            let inputs = *self
                .inputs
                .get(tick)
                .expect("input history was preflighted before rollback mutation");
            world
                .step(tick, inputs.frames())
                .map_err(RollbackError::World)?;
            ensure_world_tick::<W, S>(world, tick)?;
            let reusable = self.recycled_snapshots.pop();
            let snapshot = capture_predicted::<W, S>(world, tick, reusable)?;
            self.insert_predicted_snapshot(tick, snapshot);
            self.metrics.resimulated_ticks = self.metrics.resimulated_ticks.saturating_add(1);
        }
        if let Some(inputs) = self.inputs.get(target_tick) {
            self.last_inputs = *inputs.frames();
        }
        self.update_history_high_water();
        Ok(())
    }

    fn replace_late_input_and_repredict(&mut self, frame: InputFrame) {
        let seat = frame.seat;
        let seat_index = usize::from(seat.get());
        let mut previous = frame;
        let mut tick = frame.tick.0;
        while tick <= self.predicted_tick.0 {
            let current_tick = SimTick(tick);
            let mut inputs = *self
                .inputs
                .get(current_tick)
                .expect("late-input history was preflighted before mutation");
            if current_tick == frame.tick {
                inputs.replace_frame(frame, true);
                previous = frame;
            } else if inputs.was_predicted(seat) {
                let repeated = repeated_frame(previous, current_tick, seat_index);
                inputs.replace_frame(repeated, false);
                previous = repeated;
            } else {
                previous = inputs.frames[seat_index];
            }
            self.inputs.insert(current_tick, inputs);
            if tick == u64::MAX {
                break;
            }
            tick += 1;
        }
    }

    fn insert_predicted_snapshot(&mut self, tick: SimTick, snapshot: PredictedSnapshot<S>) {
        if let Some(expired) = self.snapshots.insert_replacing(tick, snapshot)
            && self.recycled_snapshots.len() < self.snapshots.capacity()
        {
            self.recycled_snapshots.push(expired.snapshot);
        }
    }

    fn first_missing_input_after(&self, retained_tick: SimTick) -> Option<SimTick> {
        let mut tick = retained_tick.0;
        while tick < self.predicted_tick.0 {
            tick += 1;
            let tick = SimTick(tick);
            if self.inputs.get(tick).is_none() {
                return Some(tick);
            }
        }
        None
    }

    fn require_hard_resync(&mut self, reason: HardResyncReason) -> ReconcileOutcome {
        ReconcileOutcome::HardResyncRequired(self.note_hard_resync(reason))
    }

    fn note_hard_resync(&mut self, reason: HardResyncReason) -> HardResyncReason {
        self.metrics.hard_resyncs_required = self.metrics.hard_resyncs_required.saturating_add(1);
        reason
    }

    fn record_correction_cost(
        &mut self,
        operation: RollbackOperation,
        depth: u64,
        elapsed: Duration,
    ) {
        match operation {
            RollbackOperation::AuthoritativeCorrection | RollbackOperation::LateInputCorrection => {
                self.metrics.maximum_normal_rollback_depth =
                    self.metrics.maximum_normal_rollback_depth.max(depth);
            }
            RollbackOperation::HardResync => {
                self.metrics.maximum_hard_resync_discard_depth =
                    self.metrics.maximum_hard_resync_discard_depth.max(depth);
            }
        }
        self.metrics.maximum_rollback_depth = self.metrics.maximum_rollback_depth.max(depth);
        let elapsed = elapsed.as_nanos();
        self.metrics.total_correction_nanoseconds = self
            .metrics
            .total_correction_nanoseconds
            .saturating_add(elapsed);
        self.metrics.maximum_correction_nanoseconds =
            self.metrics.maximum_correction_nanoseconds.max(elapsed);
    }

    fn update_history_high_water(&mut self) {
        self.metrics.snapshot_history_high_water = self
            .metrics
            .snapshot_history_high_water
            .max(self.snapshots.len());
        self.metrics.input_history_high_water =
            self.metrics.input_history_high_water.max(self.inputs.len());
    }
}

fn validate_frame_for(
    frame: InputFrame,
    tick: SimTick,
    seat_index: usize,
) -> Result<(), InputSetError> {
    frame.validate().map_err(InputSetError::InvalidFrame)?;
    if frame.tick != tick {
        return Err(InputSetError::WrongTick {
            expected: tick,
            found: frame.tick,
        });
    }
    if usize::from(frame.seat.get()) != seat_index {
        return Err(InputSetError::WrongSeat {
            expected: seat_index as u8,
            found: frame.seat.get(),
        });
    }
    Ok(())
}

fn neutral_frame(tick: SimTick, seat_index: usize) -> InputFrame {
    InputFrame {
        tick,
        seat: SeatId::new(seat_index as u8)
            .expect("the fixed four-seat array contains only valid seat indices"),
        ..InputFrame::default()
    }
}

fn repeated_frame(previous: InputFrame, tick: SimTick, seat_index: usize) -> InputFrame {
    InputFrame {
        tick,
        seat: SeatId::new(seat_index as u8)
            .expect("the fixed four-seat array contains only valid seat indices"),
        movement_x: previous.movement_x,
        movement_y: previous.movement_y,
        held_buttons: previous.held_buttons,
        pressed_buttons: InputButtons::default(),
        released_buttons: InputButtons::default(),
        sequence: previous.sequence,
    }
}

#[inline]
fn simulation_equivalent_input(left: InputFrame, mut right: InputFrame) -> bool {
    // InputSequence authenticates transport ordering and acknowledgement
    // progress, but gameplay systems never consume it. Preserve the accepted
    // sequence in retained history without rewinding and resimulating an
    // otherwise identical canonical input.
    right.sequence = left.sequence;
    left == right
}

fn ensure_snapshot_tick<WorldError, SnapshotHashError>(
    snapshot: &impl RollbackSnapshot<Error = SnapshotHashError>,
    expected: SimTick,
) -> Result<(), RollbackError<WorldError, SnapshotHashError>> {
    let found = snapshot.rollback_tick();
    if found == expected {
        Ok(())
    } else {
        Err(RollbackError::SnapshotTickMismatch { expected, found })
    }
}

fn ensure_world_tick<W, S>(
    world: &W,
    expected: SimTick,
) -> Result<(), RollbackError<W::Error, S::Error>>
where
    W: RollbackWorld<Snapshot = S>,
    S: RollbackSnapshot,
{
    let found = world.current_tick();
    if found == expected {
        Ok(())
    } else {
        Err(RollbackError::WorldTickMismatch { expected, found })
    }
}

fn capture_predicted<W, S>(
    world: &W,
    tick: SimTick,
    reusable: Option<S>,
) -> Result<PredictedSnapshot<S>, RollbackError<W::Error, S::Error>>
where
    W: RollbackWorld<Snapshot = S>,
    S: RollbackSnapshot,
{
    let snapshot = world
        .capture_snapshot_reusing(reusable)
        .map_err(RollbackError::World)?;
    ensure_snapshot_tick::<W::Error, S::Error>(&snapshot, tick)?;
    let hash = snapshot
        .rollback_hash()
        .map_err(RollbackError::SnapshotHash)?;
    Ok(PredictedSnapshot { snapshot, hash })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network_protocol::{InputSequence, QuantizedAxis};
    use std::convert::Infallible;

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct ToySnapshot {
        tick: SimTick,
        values: [i64; MAX_SEATS],
        edge_total: u64,
    }

    impl RollbackSnapshot for ToySnapshot {
        type Error = Infallible;

        fn rollback_tick(&self) -> SimTick {
            self.tick
        }

        fn rollback_hash(&self) -> Result<u64, Self::Error> {
            Ok(hash_toy(self))
        }
    }

    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    struct ToyWorld {
        tick: SimTick,
        values: [i64; MAX_SEATS],
        edge_total: u64,
    }

    impl RollbackWorld for ToyWorld {
        type Snapshot = ToySnapshot;
        type Error = Infallible;

        fn current_tick(&self) -> SimTick {
            self.tick
        }

        fn capture_snapshot(&self) -> Result<Self::Snapshot, Self::Error> {
            Ok(ToySnapshot {
                tick: self.tick,
                values: self.values,
                edge_total: self.edge_total,
            })
        }

        fn restore_snapshot(&mut self, snapshot: &Self::Snapshot) -> Result<(), Self::Error> {
            self.tick = snapshot.tick;
            self.values = snapshot.values;
            self.edge_total = snapshot.edge_total;
            Ok(())
        }

        fn step(
            &mut self,
            tick: SimTick,
            inputs: &[InputFrame; MAX_SEATS],
        ) -> Result<(), Self::Error> {
            self.tick = tick;
            for (seat, frame) in inputs.iter().enumerate() {
                self.values[seat] = self.values[seat]
                    .wrapping_add(i64::from(frame.movement_x.get()))
                    .wrapping_add(i64::from(frame.movement_y.get()) * 3)
                    .wrapping_add(i64::from(frame.held_buttons.bits().count_ones()) * 7);
                self.edge_total = self
                    .edge_total
                    .wrapping_add(u64::from(frame.pressed_buttons.bits().count_ones()) * 11)
                    .wrapping_add(u64::from(frame.released_buttons.bits().count_ones()) * 13);
            }
            Ok(())
        }

        fn state_hash(&self) -> Result<u64, Self::Error> {
            Ok(hash_toy(&self.capture_snapshot()?))
        }
    }

    fn hash_toy(snapshot: &ToySnapshot) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        for byte in snapshot.tick.0.to_le_bytes() {
            hash = (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3);
        }
        for value in snapshot.values {
            for byte in value.to_le_bytes() {
                hash = (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3);
            }
        }
        for byte in snapshot.edge_total.to_le_bytes() {
            hash = (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3);
        }
        hash
    }

    fn frame(
        tick: u64,
        seat: usize,
        movement: i8,
        held: u16,
        pressed: u16,
        released: u16,
    ) -> InputFrame {
        InputFrame {
            tick: SimTick(tick),
            seat: SeatId::new(seat as u8).unwrap(),
            movement_x: QuantizedAxis::new(movement).unwrap(),
            movement_y: QuantizedAxis::default(),
            held_buttons: InputButtons::new(held).unwrap(),
            pressed_buttons: InputButtons::new(pressed).unwrap(),
            released_buttons: InputButtons::new(released).unwrap(),
            sequence: InputSequence(tick as u16),
        }
    }

    fn predict_neutral(
        engine: &mut PredictionEngine<ToySnapshot>,
        world: &mut ToyWorld,
        through: u64,
    ) {
        while engine.predicted_tick().0 < through {
            engine
                .predict_next(world, [None, None, None, None])
                .unwrap();
        }
    }

    #[derive(Default)]
    struct FixedTiming {
        begins: u64,
    }

    impl RollbackTimingHook for FixedTiming {
        fn begin(&mut self, _operation: RollbackOperation, _depth_ticks: u64) {
            self.begins += 1;
        }

        fn finish(&mut self, _operation: RollbackOperation, depth_ticks: u64) -> Duration {
            Duration::from_nanos(depth_ticks * 100)
        }
    }

    #[test]
    fn canonical_snapshot_implements_production_snapshot_adapter() {
        fn require_snapshot<T: RollbackSnapshot>() {}
        require_snapshot::<CanonicalSnapshot>();
    }

    #[test]
    fn matching_authoritative_hash_is_a_no_op_and_confirmation_never_regresses() {
        let mut world = ToyWorld::default();
        let mut engine = PredictionEngine::new(&world, 32).unwrap();
        predict_neutral(&mut engine, &mut world, 5);
        let before = world.clone();
        let hash = engine.predicted_hash(SimTick(3)).unwrap();
        let mut discarded = Vec::new();
        let mut timing = FixedTiming::default();

        assert_eq!(
            engine
                .reconcile(
                    &mut world,
                    SimTick(3),
                    hash,
                    None,
                    &mut |tick| discarded.push(tick),
                    &mut timing,
                )
                .unwrap(),
            ReconcileOutcome::Matched {
                tick: SimTick(3),
                hash,
                confirmed_advanced: true,
            }
        );
        assert_eq!(world, before);
        assert!(discarded.is_empty());
        assert_eq!(timing.begins, 0);
        assert_eq!(engine.confirmed_tick(), SimTick(3));

        let old_hash = engine.predicted_hash(SimTick(2)).unwrap();
        assert_eq!(
            engine
                .reconcile(
                    &mut world,
                    SimTick(2),
                    old_hash,
                    None,
                    &mut NoopEventDiscard,
                    &mut NoopRollbackTiming,
                )
                .unwrap(),
            ReconcileOutcome::StaleAuthority {
                received_tick: SimTick(2),
                confirmed_tick: SimTick(3),
            }
        );
        assert_eq!(engine.confirmed_tick(), SimTick(3));
    }

    #[test]
    fn missing_prediction_repeats_continuous_state_and_clears_edges() {
        let mut world = ToyWorld::default();
        let mut engine = PredictionEngine::new(&world, 32).unwrap();
        let initial = frame(
            1,
            0,
            42,
            InputButtons::GUARD,
            InputButtons::LIGHT,
            InputButtons::JUMP,
        );
        engine
            .predict_next(&mut world, [Some(initial), None, None, None])
            .unwrap();
        engine
            .predict_next(&mut world, [None, None, None, None])
            .unwrap();
        let repeated = engine
            .inputs_at(SimTick(2))
            .unwrap()
            .frame(SeatId::new(0).unwrap());
        assert_eq!(repeated.movement_x.get(), 42);
        assert_eq!(repeated.held_buttons.bits(), InputButtons::GUARD);
        assert_eq!(repeated.pressed_buttons.bits(), 0);
        assert_eq!(repeated.released_buttons.bits(), 0);
        assert_eq!(repeated.sequence, initial.sequence);
        assert!(
            engine
                .inputs_at(SimTick(2))
                .unwrap()
                .was_predicted(SeatId::new(0).unwrap())
        );
    }

    #[test]
    fn mismatch_restores_authoritative_state_and_resimulates_exact_inputs() {
        let mut predicted_world = ToyWorld::default();
        let mut engine = PredictionEngine::new(&predicted_world, 32).unwrap();
        for tick in 1..=6 {
            engine
                .predict_next(
                    &mut predicted_world,
                    [Some(frame(tick, 0, 1, 0, 0, 0)), None, None, None],
                )
                .unwrap();
        }

        let authoritative = ToySnapshot {
            tick: SimTick(3),
            values: [100, 20, 30, 40],
            edge_total: 7,
        };
        let authoritative_hash = authoritative.rollback_hash().unwrap();
        let mut expected = ToyWorld::default();
        expected.restore_snapshot(&authoritative).unwrap();
        for tick in 4..=6 {
            let inputs = *engine.inputs_at(SimTick(tick)).unwrap();
            expected.step(SimTick(tick), inputs.frames()).unwrap();
        }
        let mut discarded = Vec::new();
        let mut timing = FixedTiming::default();

        assert_eq!(
            engine
                .reconcile(
                    &mut predicted_world,
                    SimTick(3),
                    authoritative_hash,
                    Some(&authoritative),
                    &mut |tick| discarded.push(tick),
                    &mut timing,
                )
                .unwrap(),
            ReconcileOutcome::Corrected {
                authoritative_tick: SimTick(3),
                resimulated_through: SimTick(6),
                depth_ticks: 3,
            }
        );
        assert_eq!(predicted_world, expected);
        assert_eq!(discarded, vec![SimTick(3)]);
        assert_eq!(
            engine.predicted_hash(SimTick(6)),
            Some(expected.state_hash().unwrap())
        );
        assert_eq!(engine.metrics().resimulated_ticks, 3);
        assert_eq!(engine.metrics().maximum_rollback_depth, 3);
        assert_eq!(engine.metrics().total_correction_nanoseconds, 300);
    }

    #[test]
    fn sequence_only_late_input_is_retained_without_rollback() {
        let mut world = ToyWorld::default();
        let mut engine = PredictionEngine::new(&world, 32).unwrap();
        predict_neutral(&mut engine, &mut world, 6);
        let confirmed_hash = engine.predicted_hash(SimTick(3)).unwrap();
        engine
            .reconcile(
                &mut world,
                SimTick(3),
                confirmed_hash,
                None,
                &mut NoopEventDiscard,
                &mut NoopRollbackTiming,
            )
            .unwrap();
        let before = world.clone();
        let mut discarded = Vec::new();
        let mut timing = FixedTiming::default();
        let accepted = frame(3, 0, 0, 0, 0, 0);

        assert_eq!(
            engine
                .apply_late_input(
                    &mut world,
                    accepted,
                    &mut |tick| discarded.push(tick),
                    &mut timing,
                )
                .unwrap(),
            LateInputOutcome::Unchanged {
                tick: SimTick(3),
                newly_known: true,
            }
        );
        assert_eq!(world, before);
        assert!(discarded.is_empty());
        assert_eq!(timing.begins, 0);
        let retained = engine.inputs_at(SimTick(3)).unwrap();
        assert_eq!(
            retained.frame(SeatId::new(0).unwrap()).sequence,
            accepted.sequence
        );
        assert!(!retained.was_predicted(SeatId::new(0).unwrap()));
        assert_eq!(engine.metrics().corrections, 0);
        assert_eq!(engine.metrics().late_input_corrections, 0);
        assert_eq!(engine.metrics().hard_resyncs_required, 0);
    }

    #[test]
    fn late_input_repredicts_following_missing_frames_and_world() {
        let mut world = ToyWorld::default();
        let mut engine = PredictionEngine::new(&world, 32).unwrap();
        predict_neutral(&mut engine, &mut world, 6);
        let mut late = frame(3, 0, 5, InputButtons::GUARD, InputButtons::LIGHT, 0);
        late.sequence = InputSequence(77);
        let mut discarded = Vec::new();

        assert_eq!(
            engine
                .apply_late_input(
                    &mut world,
                    late,
                    &mut |tick| discarded.push(tick),
                    &mut NoopRollbackTiming,
                )
                .unwrap(),
            LateInputOutcome::Corrected {
                input_tick: SimTick(3),
                restored_tick: SimTick(2),
                resimulated_through: SimTick(6),
                depth_ticks: 4,
            }
        );
        assert_eq!(discarded, vec![SimTick(2)]);
        for tick in 3..=6 {
            let corrected = engine
                .inputs_at(SimTick(tick))
                .unwrap()
                .frame(SeatId::new(0).unwrap());
            assert_eq!(corrected.movement_x.get(), 5);
            assert_eq!(corrected.held_buttons.bits(), InputButtons::GUARD);
            assert_eq!(
                corrected.pressed_buttons.bits(),
                if tick == 3 { InputButtons::LIGHT } else { 0 }
            );
        }
        assert_eq!(
            engine
                .inputs_at(SimTick(3))
                .unwrap()
                .frame(SeatId::new(0).unwrap())
                .sequence,
            InputSequence(77)
        );
        assert_eq!(engine.metrics().corrections, 1);
        assert_eq!(engine.metrics().late_input_corrections, 1);

        let mut expected = ToyWorld::default();
        for tick in 1..=6 {
            expected
                .step(
                    SimTick(tick),
                    engine.inputs_at(SimTick(tick)).unwrap().frames(),
                )
                .unwrap();
        }
        assert_eq!(world, expected);
    }

    #[test]
    fn rollback_limit_accepts_twelve_and_requires_resync_at_thirteen() {
        let mut world = ToyWorld::default();
        let mut engine = PredictionEngine::new(&world, 32).unwrap();
        predict_neutral(&mut engine, &mut world, 20);
        let at_twelve = ToySnapshot {
            tick: SimTick(8),
            values: [1, 0, 0, 0],
            edge_total: 0,
        };
        let hash = at_twelve.rollback_hash().unwrap();
        assert!(matches!(
            engine
                .reconcile(
                    &mut world,
                    SimTick(8),
                    hash,
                    Some(&at_twelve),
                    &mut NoopEventDiscard,
                    &mut NoopRollbackTiming,
                )
                .unwrap(),
            ReconcileOutcome::Corrected {
                depth_ticks: 12,
                ..
            }
        ));

        let mut world = ToyWorld::default();
        let mut engine = PredictionEngine::new(&world, 32).unwrap();
        predict_neutral(&mut engine, &mut world, 20);
        let at_thirteen = ToySnapshot {
            tick: SimTick(7),
            values: [1, 0, 0, 0],
            edge_total: 0,
        };
        let hash = at_thirteen.rollback_hash().unwrap();
        assert_eq!(
            engine
                .reconcile(
                    &mut world,
                    SimTick(7),
                    hash,
                    Some(&at_thirteen),
                    &mut NoopEventDiscard,
                    &mut NoopRollbackTiming,
                )
                .unwrap(),
            ReconcileOutcome::HardResyncRequired(HardResyncReason::RollbackDepthExceeded {
                depth_ticks: 13,
                maximum_ticks: 12,
            })
        );
    }

    #[test]
    fn expired_and_missing_history_require_hard_resync_without_mutating_world() {
        let mut world = ToyWorld::default();
        let mut engine = PredictionEngine::new(&world, 32).unwrap();
        predict_neutral(&mut engine, &mut world, 40);
        assert_eq!(engine.oldest_snapshot_tick(), Some(SimTick(9)));
        let before = world.clone();
        assert_eq!(
            engine
                .reconcile(
                    &mut world,
                    SimTick(8),
                    123,
                    None,
                    &mut NoopEventDiscard,
                    &mut NoopRollbackTiming,
                )
                .unwrap(),
            ReconcileOutcome::HardResyncRequired(HardResyncReason::SnapshotHistoryExpired {
                requested_tick: SimTick(8),
                oldest_available: Some(SimTick(9)),
            })
        );
        assert_eq!(world, before);

        let retained_tick = SimTick(35);
        let wrong_hash = engine.predicted_hash(retained_tick).unwrap() ^ 1;
        assert_eq!(
            engine
                .reconcile(
                    &mut world,
                    retained_tick,
                    wrong_hash,
                    None,
                    &mut NoopEventDiscard,
                    &mut NoopRollbackTiming,
                )
                .unwrap(),
            ReconcileOutcome::HardResyncRequired(HardResyncReason::MissingAuthoritativeSnapshot {
                tick: retained_tick,
            })
        );
    }

    #[test]
    fn hard_resync_replaces_histories_and_world_atomically_after_validation() {
        let mut world = ToyWorld::default();
        let mut engine = PredictionEngine::new(&world, 32).unwrap();
        predict_neutral(&mut engine, &mut world, 20);
        let snapshot = ToySnapshot {
            tick: SimTick(25),
            values: [9, 8, 7, 6],
            edge_total: 5,
        };
        let hash = snapshot.rollback_hash().unwrap();
        let mut discarded = Vec::new();

        assert_eq!(
            engine
                .apply_hard_resync(
                    &mut world,
                    &snapshot,
                    hash,
                    &mut |tick| discarded.push(tick),
                    &mut NoopRollbackTiming,
                )
                .unwrap(),
            HardResyncOutcome {
                authoritative_tick: SimTick(25),
                discarded_predicted_ticks: 0,
            }
        );
        assert_eq!(world.capture_snapshot().unwrap(), snapshot);
        assert_eq!(engine.predicted_tick(), SimTick(25));
        assert_eq!(engine.confirmed_tick(), SimTick(25));
        assert_eq!(engine.snapshot_history_len(), 1);
        assert_eq!(engine.input_history_len(), 0);
        assert_eq!(discarded, vec![SimTick(25)]);
    }

    #[test]
    fn corrected_result_equals_clean_authoritative_replay() {
        let mut corrected = ToyWorld::default();
        let mut engine = PredictionEngine::new(&corrected, 32).unwrap();
        for tick in 1..=10 {
            let movement = if tick <= 4 { 1 } else { 2 };
            engine
                .predict_next(
                    &mut corrected,
                    [Some(frame(tick, 0, movement, 0, 0, 0)), None, None, None],
                )
                .unwrap();
        }

        let authority_boundary = ToySnapshot {
            tick: SimTick(4),
            values: [50, 0, 0, 0],
            edge_total: 0,
        };
        engine
            .reconcile(
                &mut corrected,
                SimTick(4),
                authority_boundary.rollback_hash().unwrap(),
                Some(&authority_boundary),
                &mut NoopEventDiscard,
                &mut NoopRollbackTiming,
            )
            .unwrap();

        let mut clean = ToyWorld::default();
        clean.restore_snapshot(&authority_boundary).unwrap();
        for tick in 5..=10 {
            clean
                .step(
                    SimTick(tick),
                    engine.inputs_at(SimTick(tick)).unwrap().frames(),
                )
                .unwrap();
        }
        assert_eq!(corrected, clean);
        assert_eq!(corrected.state_hash().unwrap(), clean.state_hash().unwrap());
    }

    #[test]
    fn deterministic_hundred_thousand_tick_soak_stays_bounded() {
        let mut left_world = ToyWorld::default();
        let mut right_world = ToyWorld::default();
        let mut left = PredictionEngine::new(&left_world, 64).unwrap();
        let mut right = PredictionEngine::new(&right_world, 64).unwrap();

        for tick in 1..=100_000_u64 {
            let supplied = if tick % 5 == 0 {
                Some(frame(
                    tick,
                    0,
                    ((tick % 21) as i8) - 10,
                    if tick % 10 == 0 {
                        InputButtons::GUARD
                    } else {
                        0
                    },
                    if tick % 30 == 0 {
                        InputButtons::LIGHT
                    } else {
                        0
                    },
                    0,
                ))
            } else {
                None
            };
            left.predict_next(&mut left_world, [supplied, None, None, None])
                .unwrap();
            right
                .predict_next(&mut right_world, [supplied, None, None, None])
                .unwrap();
        }

        assert_eq!(left_world, right_world);
        assert_eq!(
            left.predicted_hash(SimTick(100_000)),
            right.predicted_hash(SimTick(100_000))
        );
        assert_eq!(left.snapshot_history_len(), 64);
        assert_eq!(left.input_history_len(), 64);
        assert_eq!(left.metrics().snapshot_history_high_water, 64);
        assert_eq!(left.metrics().input_history_high_water, 64);
        assert_eq!(left.metrics().predicted_ticks, 100_000);
    }
}
