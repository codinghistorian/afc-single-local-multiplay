//! Shared deterministic authority driver.
//!
//! The driver is deliberately independent of rendering, Steam, sockets, and wall
//! time. Offline, listen, dedicated, replay, and test runners all commit input and
//! advance a world through this same type. A pacing thread may decide *when* to
//! call [`AuthorityMatch::step`], but elapsed time is never passed into gameplay.

use crate::authority_input::{
    AuthorityInputCollector, AuthorityInputMetrics, AuthorityInputStatus,
    AuthoritySeatCommitOverride, CommitInputError, CommittedTickInputs, FrameIngestOutcome,
    InputIngestReport, InputRejectionReason,
};
use crate::network_protocol::{
    InputBatch, InputFrame, MAX_SEATS, MatchManifest, PeerId, ProtocolValidationError, SeatId,
    SeatOwner, SimTick, StateHash,
};

pub const AUTHORITY_SNAPSHOT_HISTORY_TICKS: usize = 128;

/// Minimal contract required of a complete authoritative world snapshot.
pub trait AuthoritySnapshot: Clone {
    fn tick(&self) -> SimTick;
    fn state_hash(&self) -> Result<StateHash, ProtocolValidationError>;
}

impl AuthoritySnapshot for crate::snapshot::CanonicalSnapshot {
    fn tick(&self) -> SimTick {
        self.header.tick
    }

    fn state_hash(&self) -> Result<StateHash, ProtocolValidationError> {
        self.canonical_hash()
            .map(StateHash)
            .map_err(|_| ProtocolValidationError::InvalidSnapshot)
    }
}

/// Simulation boundary shared by listen and dedicated authorities.
pub trait AuthoritySimulation {
    type Snapshot: AuthoritySnapshot;
    type Error;

    fn current_tick(&self) -> SimTick;

    /// Advances exactly `inputs.tick` and no other tick.
    fn step(&mut self, inputs: &CommittedTickInputs) -> Result<(), Self::Error>;

    fn capture_snapshot(&self) -> Result<Self::Snapshot, Self::Error>;

    /// Captures while allowing an expired history entry to donate its bounded
    /// storage. Simulations without reusable snapshot storage keep the default
    /// fresh-capture behavior.
    fn capture_snapshot_reusing(
        &self,
        reusable: Option<Self::Snapshot>,
    ) -> Result<Self::Snapshot, Self::Error> {
        drop(reusable);
        self.capture_snapshot()
    }

    /// Optionally produces the authority-owned AI tape for the next deadline.
    /// Clients, rollback worlds, and replay targets never call this hook.
    fn generate_authority_bot_frames(
        &mut self,
        _tick: SimTick,
    ) -> Result<Option<[Option<InputFrame>; MAX_SEATS]>, Self::Error> {
        Ok(None)
    }

    /// Stable non-zero result identity after the canonical match ends.
    fn final_result_id(&self) -> Option<u64> {
        None
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AuthorityMetrics {
    pub simulated_ticks: u64,
    pub snapshots_captured: u64,
    pub snapshot_history_high_water: u16,
    pub substituted_inputs: u64,
    pub result_reports: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthorityTickReport {
    pub tick: SimTick,
    pub state_hash: StateHash,
    pub committed_inputs: CommittedTickInputs,
    pub substituted_inputs: u8,
    pub final_result_id: Option<u64>,
}

#[derive(Debug)]
pub enum AuthorityMatchError<E> {
    Protocol(ProtocolValidationError),
    InputCommit(CommitInputError),
    Simulation(E),
    SimulationTickMismatch {
        expected: SimTick,
        actual: SimTick,
    },
    SnapshotTickMismatch {
        expected: SimTick,
        actual: SimTick,
    },
    BotFrameContract {
        tick: SimTick,
        seat: SeatId,
    },
    BotFrameRejected {
        tick: SimTick,
        seat: SeatId,
        reason: InputRejectionReason,
    },
    ResultIdZero,
}

impl<E> From<ProtocolValidationError> for AuthorityMatchError<E> {
    fn from(error: ProtocolValidationError) -> Self {
        Self::Protocol(error)
    }
}

impl<E> From<CommitInputError> for AuthorityMatchError<E> {
    fn from(error: CommitInputError) -> Self {
        Self::InputCommit(error)
    }
}

/// Fixed-size tick-addressed snapshot ring. New snapshots replace only the entry
/// with the same modulo slot; stale IDs are rejected by the full tick comparison.
struct AuthoritySnapshotHistory<S> {
    slots: Vec<Option<S>>,
    len: usize,
}

impl<S: AuthoritySnapshot> AuthoritySnapshotHistory<S> {
    fn new() -> Self {
        Self {
            slots: std::iter::repeat_with(|| None)
                .take(AUTHORITY_SNAPSHOT_HISTORY_TICKS)
                .collect(),
            len: 0,
        }
    }

    const fn slot(tick: SimTick) -> usize {
        tick.0 as usize % AUTHORITY_SNAPSHOT_HISTORY_TICKS
    }

    fn insert(&mut self, snapshot: S) {
        let slot = Self::slot(snapshot.tick());
        if self.slots[slot].is_none() {
            self.len += 1;
        }
        self.slots[slot] = Some(snapshot);
    }

    fn take_reusable(&mut self, tick: SimTick) -> Option<S> {
        let snapshot = self.slots[Self::slot(tick)].take();
        if snapshot.is_some() {
            self.len -= 1;
        }
        snapshot
    }

    fn get(&self, tick: SimTick) -> Option<&S> {
        self.slots[Self::slot(tick)]
            .as_ref()
            .filter(|snapshot| snapshot.tick() == tick)
    }

    const fn len(&self) -> usize {
        self.len
    }
}

/// One canonical match authority. This type performs no I/O and owns no clock.
pub struct AuthorityMatch<S: AuthoritySimulation> {
    manifest: MatchManifest,
    simulation: S,
    inputs: AuthorityInputCollector,
    snapshots: AuthoritySnapshotHistory<S::Snapshot>,
    metrics: AuthorityMetrics,
    reported_result_id: Option<u64>,
}

impl<S: AuthoritySimulation> AuthorityMatch<S> {
    pub fn new(
        manifest: MatchManifest,
        simulation: S,
        input_config: crate::authority_input::AuthorityInputConfig,
    ) -> Result<Self, AuthorityMatchError<S::Error>> {
        manifest.validate()?;
        input_config.validate_for_manifest(&manifest)?;
        let initial_tick = simulation.current_tick();
        let first_commit_tick = initial_tick
            .0
            .checked_add(1)
            .map(SimTick)
            .ok_or(CommitInputError::TimelineExhausted)?;
        let inputs = AuthorityInputCollector::new(
            manifest.match_id,
            manifest.ownership,
            first_commit_tick,
            input_config,
        )?;
        let initial_snapshot = simulation
            .capture_snapshot()
            .map_err(AuthorityMatchError::Simulation)?;
        if initial_snapshot.tick() != initial_tick {
            return Err(AuthorityMatchError::SnapshotTickMismatch {
                expected: initial_tick,
                actual: initial_snapshot.tick(),
            });
        }
        initial_snapshot.state_hash()?;
        let mut snapshots = AuthoritySnapshotHistory::new();
        snapshots.insert(initial_snapshot);

        Ok(Self {
            manifest,
            simulation,
            inputs,
            snapshots,
            metrics: AuthorityMetrics {
                snapshots_captured: 1,
                snapshot_history_high_water: 1,
                ..AuthorityMetrics::default()
            },
            reported_result_id: None,
        })
    }

    pub const fn manifest(&self) -> &MatchManifest {
        &self.manifest
    }

    pub const fn simulation(&self) -> &S {
        &self.simulation
    }

    pub fn simulation_mut(&mut self) -> &mut S {
        &mut self.simulation
    }

    pub const fn metrics(&self) -> AuthorityMetrics {
        self.metrics
    }

    pub const fn input_metrics(&self) -> &AuthorityInputMetrics {
        self.inputs.metrics()
    }

    /// Latest processed-input cursors for transport state acknowledgements.
    ///
    /// Keeping this as a read-only projection prevents session runners from
    /// reaching into the collector or inventing a second acknowledgement path.
    pub fn processed_input_acknowledgement(
        &self,
    ) -> crate::authority_input::ProcessedInputAcknowledgement {
        self.inputs.acknowledgement()
    }

    pub fn snapshot_at(&self, tick: SimTick) -> Option<&S::Snapshot> {
        self.snapshots.get(tick)
    }

    pub fn ingest_peer_batch(
        &mut self,
        peer: PeerId,
        batch: &InputBatch,
    ) -> Result<InputIngestReport, ProtocolValidationError> {
        self.inputs.ingest_peer_batch(peer, batch)
    }

    /// Rebinds sequence validation to a newly authenticated connection
    /// generation without discarding committed canonical input history.
    pub fn begin_peer_input_epoch(
        &mut self,
        peer: PeerId,
        first_tick: SimTick,
    ) -> Result<(), ProtocolValidationError> {
        self.inputs.begin_peer_input_epoch(peer, first_tick)
    }

    pub fn ingest_bot_frame(&mut self, frame: InputFrame) -> FrameIngestOutcome {
        self.inputs.ingest_bot_frame(frame)
    }

    /// Read-only startup readiness check. Normal live ticks never wait for
    /// input, but a match must not consume synthetic missing frames before the
    /// first authenticated input window has crossed the agreed countdown.
    pub fn has_buffered_input(&self, seat: SeatId, tick: SimTick) -> bool {
        self.inputs
            .history_at(seat, tick)
            .is_some_and(|record| record.status == AuthorityInputStatus::Buffered)
    }

    fn ingest_generated_bot_frames(
        &mut self,
        tick: SimTick,
        frames: [Option<InputFrame>; MAX_SEATS],
    ) -> Result<(), AuthorityMatchError<S::Error>> {
        for seat_index in 0..MAX_SEATS {
            let seat = SeatId::new(seat_index as u8)
                .expect("the fixed bot input array contains valid protocol seats");
            let assignment = self.manifest.ownership.assignment_for_seat(seat);
            let expects_bot = assignment
                .is_some_and(|assignment| matches!(assignment.owner, SeatOwner::AuthorityBot));
            let Some(frame) = frames[seat_index] else {
                if expects_bot {
                    return Err(AuthorityMatchError::BotFrameContract { tick, seat });
                }
                continue;
            };
            if !expects_bot || frame.tick != tick || frame.seat != seat {
                return Err(AuthorityMatchError::BotFrameContract { tick, seat });
            }
            frame.validate()?;
            match self.ingest_bot_frame(frame) {
                FrameIngestOutcome::Accepted { .. } => {}
                FrameIngestOutcome::Rejected(InputRejectionReason::Duplicate)
                    if self.inputs.history_at(seat, tick).is_some_and(|record| {
                        record.frame == frame
                            && matches!(
                                record.origin,
                                crate::authority_input::AuthorityInputOrigin::AuthorityBot
                            )
                    }) => {}
                FrameIngestOutcome::Rejected(reason) => {
                    return Err(AuthorityMatchError::BotFrameRejected { tick, seat, reason });
                }
            }
        }
        Ok(())
    }

    pub fn step(&mut self) -> Result<AuthorityTickReport, AuthorityMatchError<S::Error>> {
        self.step_with_overrides(&[AuthoritySeatCommitOverride::Normal; MAX_SEATS])
    }

    /// Advances one canonical tick while applying authority-owned reconnect
    /// substitution policy to peer seats. Normal match code calls [`Self::step`].
    pub fn step_with_overrides(
        &mut self,
        overrides: &[AuthoritySeatCommitOverride; MAX_SEATS],
    ) -> Result<AuthorityTickReport, AuthorityMatchError<S::Error>> {
        let tick = self
            .inputs
            .next_commit_tick()
            .ok_or(CommitInputError::TimelineExhausted)?;
        if let Some(frames) = self
            .simulation
            .generate_authority_bot_frames(tick)
            .map_err(AuthorityMatchError::Simulation)?
        {
            self.ingest_generated_bot_frames(tick, frames)?;
        }
        let committed_inputs = self.inputs.commit_tick_with_overrides(tick, overrides)?;
        let substituted_inputs = committed_inputs
            .iter()
            .filter(|record| record.was_substituted())
            .count() as u8;

        self.simulation
            .step(&committed_inputs)
            .map_err(AuthorityMatchError::Simulation)?;
        let actual_tick = self.simulation.current_tick();
        if actual_tick != tick {
            return Err(AuthorityMatchError::SimulationTickMismatch {
                expected: tick,
                actual: actual_tick,
            });
        }

        let reusable = self.snapshots.take_reusable(tick);
        let snapshot = self
            .simulation
            .capture_snapshot_reusing(reusable)
            .map_err(AuthorityMatchError::Simulation)?;
        if snapshot.tick() != tick {
            return Err(AuthorityMatchError::SnapshotTickMismatch {
                expected: tick,
                actual: snapshot.tick(),
            });
        }
        let state_hash = snapshot.state_hash()?;
        self.snapshots.insert(snapshot);

        let final_result_id = self.simulation.final_result_id();
        if final_result_id == Some(0) {
            return Err(AuthorityMatchError::ResultIdZero);
        }
        if let Some(result_id) = final_result_id
            && self.reported_result_id != Some(result_id)
        {
            self.reported_result_id = Some(result_id);
            self.metrics.result_reports = self.metrics.result_reports.saturating_add(1);
        }

        self.metrics.simulated_ticks = self.metrics.simulated_ticks.saturating_add(1);
        self.metrics.snapshots_captured = self.metrics.snapshots_captured.saturating_add(1);
        self.metrics.substituted_inputs = self
            .metrics
            .substituted_inputs
            .saturating_add(u64::from(substituted_inputs));
        self.metrics.snapshot_history_high_water = self
            .metrics
            .snapshot_history_high_water
            .max(self.snapshots.len() as u16);

        Ok(AuthorityTickReport {
            tick,
            state_hash,
            committed_inputs,
            substituted_inputs,
            final_result_id,
        })
    }

    /// Advances without sleeping. Replay, tests, and headless batch verification
    /// use this path to run faster than realtime.
    pub fn run_ticks(
        &mut self,
        count: u32,
        mut observe: impl FnMut(&AuthorityTickReport),
    ) -> Result<(), AuthorityMatchError<S::Error>> {
        for _ in 0..count {
            let report = self.step()?;
            observe(&report);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    use crate::authority_input::{AuthorityInputConfig, AuthorityInputOrigin};
    use crate::network_protocol::{
        AuthorityKind, BuildId, CompatibilityId, DefinitionId, FighterId, FighterSlotConfig,
        GameplayContentHash, InputButtons, InputSequence, ManifestHash, MatchId, QuantizedAxis,
        ReplayFormatVersion, SeatAssignment, SeatId, SeatOwner, SeatOwnership, SimulationVersion,
        TeamId,
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
        generate_bot_frames: bool,
        reused_snapshots: Cell<u64>,
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
            for record in inputs.iter() {
                self.hash = self
                    .hash
                    .wrapping_mul(1_099_511_628_211)
                    .wrapping_add(record.frame.movement_x.get() as i64 as u64)
                    .wrapping_add(u64::from(record.frame.held_buttons.bits()));
            }
            Ok(())
        }

        fn capture_snapshot(&self) -> Result<Self::Snapshot, Self::Error> {
            Ok(ToySnapshot {
                tick: self.tick,
                hash: self.hash,
            })
        }

        fn capture_snapshot_reusing(
            &self,
            reusable: Option<Self::Snapshot>,
        ) -> Result<Self::Snapshot, Self::Error> {
            if let Some(mut snapshot) = reusable {
                snapshot.tick = self.tick;
                snapshot.hash = self.hash;
                self.reused_snapshots
                    .set(self.reused_snapshots.get().saturating_add(1));
                Ok(snapshot)
            } else {
                self.capture_snapshot()
            }
        }

        fn generate_authority_bot_frames(
            &mut self,
            tick: SimTick,
        ) -> Result<Option<[Option<InputFrame>; MAX_SEATS]>, Self::Error> {
            if !self.generate_bot_frames {
                return Ok(None);
            }
            let mut frames = [None; MAX_SEATS];
            frames[1] = Some(InputFrame {
                tick,
                seat: SeatId::new(1).unwrap(),
                movement_x: QuantizedAxis::new(-48).unwrap(),
                movement_y: QuantizedAxis::default(),
                held_buttons: InputButtons::default(),
                pressed_buttons: InputButtons::new(InputButtons::LIGHT).unwrap(),
                released_buttons: InputButtons::default(),
                sequence: InputSequence(tick.0 as u16),
            });
            Ok(Some(frames))
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
        let ownership = SeatOwnership::from_assignments(&[
            SeatAssignment {
                seat: SeatId::new(0).unwrap(),
                fighter: FighterId::new(0).unwrap(),
                owner: SeatOwner::Peer(peer()),
            },
            SeatAssignment {
                seat: SeatId::new(1).unwrap(),
                fighter: FighterId::new(1).unwrap(),
                owner: SeatOwner::AuthorityBot,
            },
        ])
        .unwrap();
        let mut slots = [FighterSlotConfig::default(); 4];
        for index in 0..2 {
            slots[index] = FighterSlotConfig {
                occupied: true,
                fighter: FighterId::new(index as u8).unwrap(),
                team: TeamId::new(index as u8).unwrap(),
                character: DefinitionId::new(index as u16).unwrap(),
                style: DefinitionId::new(1).unwrap(),
                equipment: DefinitionId::new(0).unwrap(),
            };
        }
        MatchManifest {
            compatibility: CompatibilityId {
                protocol: crate::network_protocol::ProtocolVersion::new(1).unwrap(),
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

    fn input(tick: u64, seat: u8, sequence: u16) -> InputFrame {
        InputFrame {
            tick: SimTick(tick),
            seat: SeatId::new(seat).unwrap(),
            movement_x: QuantizedAxis::new(32).unwrap(),
            movement_y: QuantizedAxis::default(),
            held_buttons: InputButtons::new(InputButtons::LIGHT).unwrap(),
            pressed_buttons: InputButtons::new(InputButtons::LIGHT).unwrap(),
            released_buttons: InputButtons::default(),
            sequence: InputSequence(sequence),
        }
    }

    fn authority(finish_tick: Option<u64>) -> AuthorityMatch<ToySimulation> {
        AuthorityMatch::new(
            manifest(),
            ToySimulation {
                tick: SimTick::ZERO,
                hash: 17,
                finish_tick: finish_tick.map(SimTick),
                generate_bot_frames: false,
                reused_snapshots: Cell::new(0),
            },
            AuthorityInputConfig::default(),
        )
        .unwrap()
    }

    #[test]
    fn authority_future_input_window_covers_negotiated_delay_plus_one() {
        let mut delayed_manifest = manifest();
        delayed_manifest.input_delay_ticks = 6;
        let simulation = || ToySimulation {
            tick: SimTick::ZERO,
            hash: 17,
            finish_tick: None,
            generate_bot_frames: false,
            reused_snapshots: Cell::new(0),
        };
        let too_small = AuthorityInputConfig {
            max_future_ticks: 6,
            ..AuthorityInputConfig::default()
        };
        assert!(matches!(
            AuthorityMatch::new(delayed_manifest, simulation(), too_small),
            Err(AuthorityMatchError::Protocol(
                ProtocolValidationError::InvalidTickWindow
            ))
        ));

        let exact = AuthorityInputConfig {
            max_future_ticks: 7,
            ..AuthorityInputConfig::default()
        };
        assert!(AuthorityMatch::new(delayed_manifest, simulation(), exact).is_ok());
    }

    #[test]
    fn peer_and_bot_inputs_share_the_same_commit_path() {
        let mut authority = authority(None);
        let human =
            crate::network_protocol::SeatInputWindow::from_newest_first(&[input(1, 0, 1)]).unwrap();
        let batch = InputBatch::new(match_id(), peer(), &[human]).unwrap();
        assert_eq!(
            authority
                .ingest_peer_batch(peer(), &batch)
                .unwrap()
                .accepted,
            1
        );
        assert!(matches!(
            authority.ingest_bot_frame(input(1, 1, 1)),
            FrameIngestOutcome::Accepted { .. }
        ));

        let report = authority.step().unwrap();
        assert_eq!(report.tick, SimTick(1));
        assert_eq!(report.substituted_inputs, 0);
        let origins: Vec<_> = report
            .committed_inputs
            .iter()
            .map(|record| record.origin)
            .collect();
        assert_eq!(
            origins,
            vec![
                AuthorityInputOrigin::Peer(peer()),
                AuthorityInputOrigin::AuthorityBot
            ]
        );
    }

    #[test]
    fn generated_bot_tape_commits_with_authority_origin_and_replay_safe_frames() {
        let mut authority = authority(None);
        authority.simulation_mut().generate_bot_frames = true;

        for tick in 1..=2 {
            let human = crate::network_protocol::SeatInputWindow::from_newest_first(&[input(
                tick,
                0,
                tick as u16,
            )])
            .unwrap();
            let batch = InputBatch::new(match_id(), peer(), &[human]).unwrap();
            assert_eq!(
                authority
                    .ingest_peer_batch(peer(), &batch)
                    .unwrap()
                    .accepted,
                1
            );

            let report = authority.step().unwrap();
            let bot = report.committed_inputs.by_seat[1].unwrap();
            assert_eq!(bot.origin, AuthorityInputOrigin::AuthorityBot);
            assert_eq!(
                bot.status,
                crate::authority_input::AuthorityInputStatus::Committed
            );
            assert_eq!(bot.frame.tick, SimTick(tick));
            assert_eq!(bot.frame.sequence, InputSequence(tick as u16));
            bot.frame.validate().unwrap();
            assert!(!bot.was_substituted());
        }
        assert_eq!(authority.input_metrics().accepted_bot_frames, 2);
        assert_eq!(authority.metrics().substituted_inputs, 0);
    }

    #[test]
    fn missing_inputs_are_substituted_and_observable() {
        let mut authority = authority(None);
        let report = authority.step().unwrap();
        assert_eq!(report.substituted_inputs, 2);
        assert_eq!(authority.metrics().substituted_inputs, 2);
        assert_eq!(authority.input_metrics().substituted_frames, 2);
    }

    #[test]
    fn snapshots_are_tick_addressed_and_history_is_bounded() {
        let mut authority = authority(None);
        authority.run_ticks(200, |_| {}).unwrap();
        assert!(authority.snapshot_at(SimTick(200)).is_some());
        assert!(authority.snapshot_at(SimTick(73)).is_some());
        assert!(authority.snapshot_at(SimTick(72)).is_none());
        assert_eq!(
            authority.metrics().snapshot_history_high_water as usize,
            AUTHORITY_SNAPSHOT_HISTORY_TICKS
        );
        assert_eq!(
            authority.simulation().reused_snapshots.get(),
            200 - (AUTHORITY_SNAPSHOT_HISTORY_TICKS as u64 - 1)
        );
    }

    #[test]
    fn final_result_is_reported_idempotently() {
        let mut authority = authority(Some(3));
        let mut reports = Vec::new();
        authority
            .run_ticks(5, |report| reports.push(report.final_result_id))
            .unwrap();
        assert_eq!(reports, vec![None, None, Some(77), None, None]);
        assert_eq!(authority.metrics().result_reports, 1);
    }

    #[test]
    fn authority_runs_faster_than_realtime_without_a_time_resource() {
        let mut authority = authority(None);
        let mut last = SimTick::ZERO;
        authority
            .run_ticks(10_000, |report| last = report.tick)
            .unwrap();
        assert_eq!(last, SimTick(10_000));
        assert_eq!(authority.metrics().simulated_ticks, 10_000);
    }
}
