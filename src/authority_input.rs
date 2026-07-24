//! Authority-side input acceptance, history, and deadline commitment.
//!
//! The authority never rewrites a committed tick. Human and bot frames enter the
//! same bounded per-seat history; at a tick deadline, an absent frame is replaced
//! with the previous continuous state while edge pulses are cleared.

use crate::network_protocol::{
    FighterId, InputBatch, InputButtons, InputFrame, InputSequence, InputTickWindow,
    MAX_INPUT_REDUNDANCY, MAX_SEATS, MatchId, MatchManifest, PeerId, ProtocolResult,
    ProtocolValidationError, SeatAssignment, SeatId, SeatOwner, SeatOwnership, SimTick,
};

/// Slots retained per seat for committed history plus bounded future input.
///
/// This is deliberately a power of two, although indexing does not rely on it.
pub const AUTHORITY_INPUT_HISTORY_CAPACITY: usize = 128;

/// Number of already-committed ticks kept queryable per occupied seat.
pub const AUTHORITY_INPUT_RETENTION_TICKS: u64 = 64;

/// Conservative default lead accepted ahead of the next authority deadline.
pub const DEFAULT_MAX_FUTURE_INPUT_TICKS: u64 = 16;

pub const DEFAULT_ABUSE_WARNING_THRESHOLD: u32 = 16;
pub const DEFAULT_ABUSE_DISCONNECT_THRESHOLD: u32 = 64;

const MAX_CONFIGURED_FUTURE_TICKS: u64 =
    AUTHORITY_INPUT_HISTORY_CAPACITY as u64 - AUTHORITY_INPUT_RETENTION_TICKS - 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthorityInputConfig {
    pub max_future_ticks: u64,
    pub abuse_warning_threshold: u32,
    pub abuse_disconnect_threshold: u32,
}

impl Default for AuthorityInputConfig {
    fn default() -> Self {
        Self {
            max_future_ticks: DEFAULT_MAX_FUTURE_INPUT_TICKS,
            abuse_warning_threshold: DEFAULT_ABUSE_WARNING_THRESHOLD,
            abuse_disconnect_threshold: DEFAULT_ABUSE_DISCONNECT_THRESHOLD,
        }
    }
}

impl AuthorityInputConfig {
    pub fn validate(self) -> ProtocolResult<()> {
        if self.max_future_ticks > MAX_CONFIGURED_FUTURE_TICKS
            || self.abuse_warning_threshold == 0
            || self.abuse_disconnect_threshold < self.abuse_warning_threshold
        {
            Err(ProtocolValidationError::InvalidTickWindow)
        } else {
            Ok(())
        }
    }

    pub fn validate_for_manifest(self, manifest: &MatchManifest) -> ProtocolResult<()> {
        self.validate()?;
        if self.max_future_ticks < u64::from(manifest.input_delay_ticks).saturating_add(1) {
            Err(ProtocolValidationError::InvalidTickWindow)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AuthorityInputOrigin {
    Peer(PeerId),
    AuthorityBot,
    /// Authority-generated control for a temporarily disconnected peer-owned
    /// seat. It is canonical and replayable, but never advances that peer's
    /// processed-input acknowledgement.
    DisconnectedBot(PeerId),
    #[default]
    MissingSubstitute,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AuthorityInputStatus {
    #[default]
    Buffered,
    Committed,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AuthorityInputRecord {
    pub frame: InputFrame,
    pub fighter: FighterId,
    pub origin: AuthorityInputOrigin,
    pub status: AuthorityInputStatus,
}

impl AuthorityInputRecord {
    pub const fn was_substituted(self) -> bool {
        matches!(
            self.origin,
            AuthorityInputOrigin::MissingSubstitute | AuthorityInputOrigin::DisconnectedBot(_)
        )
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AuthoritySeatCommitOverride {
    /// Consume the accepted peer/bot frame, or repeat the prior continuous
    /// state with edge pulses cleared when the deadline is missed.
    #[default]
    Normal,
    /// Ignore any buffered frame for this peer-owned seat and commit fully
    /// neutral input. Used during the first reconnect-grace interval.
    ForceNeutral,
    /// Ignore any buffered peer frame and commit this authority-generated bot
    /// frame for the named disconnected owner.
    DisconnectedBot { peer_id: PeerId, frame: InputFrame },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputRejectionReason {
    Invalid,
    UnownedSeat,
    SeatOwnedByDifferentPeer,
    AuthorityOwnedSeat,
    Stale,
    Future,
    Duplicate,
    CommittedLate,
    SequenceOutsideWindow,
    ConflictingFrame,
    HistoryCapacity,
}

impl InputRejectionReason {
    const fn counts_as_abuse(self) -> bool {
        !matches!(self, Self::Duplicate)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputRejectionCounts {
    pub invalid: u16,
    pub unowned: u16,
    pub stale: u16,
    pub future: u16,
    pub duplicate: u16,
    pub committed_late: u16,
    pub sequence: u16,
    pub conflicting: u16,
    pub capacity: u16,
}

impl InputRejectionCounts {
    fn record(&mut self, reason: InputRejectionReason) {
        let counter = match reason {
            InputRejectionReason::Invalid => &mut self.invalid,
            InputRejectionReason::UnownedSeat
            | InputRejectionReason::SeatOwnedByDifferentPeer
            | InputRejectionReason::AuthorityOwnedSeat => &mut self.unowned,
            InputRejectionReason::Stale => &mut self.stale,
            InputRejectionReason::Future => &mut self.future,
            InputRejectionReason::Duplicate => &mut self.duplicate,
            InputRejectionReason::CommittedLate => &mut self.committed_late,
            InputRejectionReason::SequenceOutsideWindow => &mut self.sequence,
            InputRejectionReason::ConflictingFrame => &mut self.conflicting,
            InputRejectionReason::HistoryCapacity => &mut self.capacity,
        };
        *counter = counter.saturating_add(1);
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputIngestReport {
    pub accepted: u16,
    pub redundant_recoveries: u16,
    pub rejected: u16,
    pub rejections: InputRejectionCounts,
}

impl InputIngestReport {
    fn record(&mut self, outcome: FrameIngestOutcome) {
        match outcome {
            FrameIngestOutcome::Accepted { redundant } => {
                self.accepted = self.accepted.saturating_add(1);
                if redundant {
                    self.redundant_recoveries = self.redundant_recoveries.saturating_add(1);
                }
            }
            FrameIngestOutcome::Rejected(reason) => {
                self.rejected = self.rejected.saturating_add(1);
                self.rejections.record(reason);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameIngestOutcome {
    Accepted { redundant: bool },
    Rejected(InputRejectionReason),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AuthorityInputMetrics {
    pub accepted_frames: u64,
    pub accepted_peer_frames: u64,
    pub accepted_bot_frames: u64,
    pub redundant_recoveries: u64,
    pub rejected_frames: u64,
    pub rejected_invalid_frames: u64,
    pub rejected_unowned_frames: u64,
    pub rejected_stale_frames: u64,
    pub rejected_future_frames: u64,
    pub rejected_duplicate_frames: u64,
    pub rejected_committed_late_frames: u64,
    pub rejected_sequence_frames: u64,
    pub rejected_conflicting_frames: u64,
    pub rejected_capacity_frames: u64,
    pub rejected_invalid_batches: u64,
    pub rejected_match_batches: u64,
    pub rejected_peer_batches: u64,
    pub committed_frames: u64,
    pub substituted_frames: u64,
    pub maximum_pending_frames: u16,
    pub maximum_retained_frames: u16,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputCursor {
    pub tick: SimTick,
    pub sequence: InputSequence,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SeatInputHighWater {
    pub accepted: Option<InputCursor>,
    pub processed: Option<InputCursor>,
    pub committed_through: Option<SimTick>,
    pub pending_frames: u16,
    pub retained_frames: u16,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SeatProcessedInputAcknowledgement {
    pub seat: SeatId,
    pub fighter: FighterId,
    /// The authority has simulated this tick, even if it substituted input.
    pub processed_through: Option<SimTick>,
    /// Latest received (non-substituted) frame consumed by simulation.
    pub processed_input: Option<InputCursor>,
    /// Latest received frame currently retained, including future buffered input.
    pub accepted_high_water: Option<InputCursor>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessedInputAcknowledgement {
    pub match_id: MatchId,
    pub authority_tick: Option<SimTick>,
    count: u8,
    seats: [SeatProcessedInputAcknowledgement; MAX_SEATS],
}

impl ProcessedInputAcknowledgement {
    pub const fn len(&self) -> usize {
        self.count as usize
    }

    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn as_slice(&self) -> &[SeatProcessedInputAcknowledgement] {
        &self.seats[..self.len().min(MAX_SEATS)]
    }

    pub fn for_seat(&self, seat: SeatId) -> Option<&SeatProcessedInputAcknowledgement> {
        self.as_slice().iter().find(|entry| entry.seat == seat)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommittedTickInputs {
    pub tick: SimTick,
    pub by_seat: [Option<AuthorityInputRecord>; MAX_SEATS],
}

impl CommittedTickInputs {
    pub fn for_seat(&self, seat: SeatId) -> Option<&AuthorityInputRecord> {
        self.by_seat[usize::from(seat.get())].as_ref()
    }

    pub fn len(&self) -> usize {
        self.by_seat.iter().flatten().count()
    }

    pub fn is_empty(&self) -> bool {
        self.by_seat.iter().all(Option::is_none)
    }

    pub fn iter(&self) -> impl Iterator<Item = &AuthorityInputRecord> {
        self.by_seat.iter().flatten()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitInputError {
    AlreadyCommitted {
        requested: SimTick,
        next_expected: SimTick,
    },
    TickGap {
        requested: SimTick,
        next_expected: SimTick,
    },
    TimelineExhausted,
    HistoryCapacity,
    OverrideForUnownedSeat(SeatId),
    OverrideForAuthorityBot(SeatId),
    OverrideOwnerMismatch(SeatId),
    OverrideFrameMismatch(SeatId),
    InvalidOverrideFrame(SeatId),
}

impl core::fmt::Display for CommitInputError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "authority input commit failed: {self:?}")
    }
}

impl std::error::Error for CommitInputError {}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum AbuseSignal {
    #[default]
    None,
    Warning,
    Disconnect,
}

#[derive(Clone, Copy, Debug)]
struct PeerAbuseState {
    peer: Option<PeerId>,
    violations: u32,
    emitted: AbuseSignal,
}

impl PeerAbuseState {
    const EMPTY: Self = Self {
        peer: None,
        violations: 0,
        emitted: AbuseSignal::None,
    };
}

struct SeatInputHistory {
    slots: [Option<AuthorityInputRecord>; AUTHORITY_INPUT_HISTORY_CAPACITY],
    accepted_high_water: Option<InputCursor>,
    processed_high_water: Option<InputCursor>,
    sequence_epoch_start: SimTick,
    last_committed_frame: Option<InputFrame>,
    retained_count: u16,
    pending_count: u16,
}

impl SeatInputHistory {
    const fn empty() -> Self {
        Self {
            slots: [None; AUTHORITY_INPUT_HISTORY_CAPACITY],
            accepted_high_water: None,
            processed_high_water: None,
            sequence_epoch_start: SimTick::ZERO,
            last_committed_frame: None,
            retained_count: 0,
            pending_count: 0,
        }
    }

    const fn slot_index(tick: SimTick) -> usize {
        (tick.0 % AUTHORITY_INPUT_HISTORY_CAPACITY as u64) as usize
    }

    fn get(&self, tick: SimTick) -> Option<&AuthorityInputRecord> {
        self.slots[Self::slot_index(tick)]
            .as_ref()
            .filter(|record| record.frame.tick == tick)
    }

    fn contains_received_sequence(&self, sequence: InputSequence) -> bool {
        self.slots.iter().flatten().any(|record| {
            record.frame.tick >= self.sequence_epoch_start
                && !record.was_substituted()
                && record.frame.sequence == sequence
        })
    }

    fn begin_sequence_epoch(&mut self, first_tick: SimTick) {
        for slot in &mut self.slots {
            if slot.is_some_and(|record| {
                record.status == AuthorityInputStatus::Buffered && record.frame.tick >= first_tick
            }) {
                *slot = None;
                self.pending_count = self.pending_count.saturating_sub(1);
                self.retained_count = self.retained_count.saturating_sub(1);
            }
        }
        self.accepted_high_water = None;
        self.sequence_epoch_start = first_tick;
    }

    fn slot_available_for(&self, tick: SimTick) -> bool {
        self.slots[Self::slot_index(tick)]
            .as_ref()
            .is_none_or(|record| record.frame.tick == tick)
    }

    fn insert_buffered(&mut self, record: AuthorityInputRecord) -> bool {
        let index = Self::slot_index(record.frame.tick);
        if self.slots[index]
            .as_ref()
            .is_some_and(|existing| existing.frame.tick != record.frame.tick)
        {
            return false;
        }
        if self.slots[index].is_none() {
            self.retained_count = self.retained_count.saturating_add(1);
            self.pending_count = self.pending_count.saturating_add(1);
        }
        self.slots[index] = Some(record);
        true
    }

    fn insert_committed(&mut self, record: AuthorityInputRecord) -> bool {
        let index = Self::slot_index(record.frame.tick);
        if self.slots[index]
            .as_ref()
            .is_some_and(|existing| existing.frame.tick != record.frame.tick)
        {
            return false;
        }
        if self.slots[index].is_none() {
            self.retained_count = self.retained_count.saturating_add(1);
        } else if self.slots[index]
            .as_ref()
            .is_some_and(|existing| existing.status == AuthorityInputStatus::Buffered)
        {
            self.pending_count = self.pending_count.saturating_sub(1);
        }
        self.slots[index] = Some(record);
        true
    }

    fn mark_committed(&mut self, tick: SimTick) -> Option<AuthorityInputRecord> {
        let index = Self::slot_index(tick);
        let record = self.slots[index].as_mut()?;
        if record.frame.tick != tick {
            return None;
        }
        if record.status == AuthorityInputStatus::Buffered {
            record.status = AuthorityInputStatus::Committed;
            self.pending_count = self.pending_count.saturating_sub(1);
        }
        Some(*record)
    }

    fn prune_before(&mut self, oldest_retained: SimTick) {
        for slot in &mut self.slots {
            let Some(record) = slot else {
                continue;
            };
            if record.frame.tick.0 >= oldest_retained.0 {
                continue;
            }
            self.retained_count = self.retained_count.saturating_sub(1);
            if record.status == AuthorityInputStatus::Buffered {
                self.pending_count = self.pending_count.saturating_sub(1);
            }
            *slot = None;
        }
    }
}

/// Fixed-capacity authority input collector for at most four seats.
pub struct AuthorityInputCollector {
    match_id: MatchId,
    ownership: SeatOwnership,
    assignments: [Option<SeatAssignment>; MAX_SEATS],
    histories: [SeatInputHistory; MAX_SEATS],
    peer_abuse: [PeerAbuseState; MAX_SEATS],
    next_commit_tick: Option<SimTick>,
    last_committed_tick: Option<SimTick>,
    config: AuthorityInputConfig,
    metrics: AuthorityInputMetrics,
}

impl AuthorityInputCollector {
    pub fn new(
        match_id: MatchId,
        ownership: SeatOwnership,
        first_commit_tick: SimTick,
        config: AuthorityInputConfig,
    ) -> ProtocolResult<Self> {
        match_id.validate()?;
        ownership.validate()?;
        config.validate()?;

        let mut assignments = [None; MAX_SEATS];
        let mut peer_abuse = [PeerAbuseState::EMPTY; MAX_SEATS];
        let mut peer_count = 0usize;

        for assignment in ownership.as_slice() {
            assignments[usize::from(assignment.seat.get())] = Some(*assignment);
            let SeatOwner::Peer(peer) = assignment.owner else {
                continue;
            };
            if peer_abuse[..peer_count]
                .iter()
                .any(|state| state.peer == Some(peer))
            {
                continue;
            }
            peer_abuse[peer_count].peer = Some(peer);
            peer_count += 1;
        }

        Ok(Self {
            match_id,
            ownership,
            assignments,
            histories: [
                SeatInputHistory::empty(),
                SeatInputHistory::empty(),
                SeatInputHistory::empty(),
                SeatInputHistory::empty(),
            ],
            peer_abuse,
            next_commit_tick: Some(first_commit_tick),
            last_committed_tick: None,
            config,
            metrics: AuthorityInputMetrics::default(),
        })
    }

    pub const fn match_id(&self) -> MatchId {
        self.match_id
    }

    pub const fn ownership(&self) -> &SeatOwnership {
        &self.ownership
    }

    pub const fn next_commit_tick(&self) -> Option<SimTick> {
        self.next_commit_tick
    }

    pub const fn last_committed_tick(&self) -> Option<SimTick> {
        self.last_committed_tick
    }

    pub const fn metrics(&self) -> &AuthorityInputMetrics {
        &self.metrics
    }

    pub fn tick_window(&self) -> Option<InputTickWindow> {
        let first_uncommitted = self.next_commit_tick?;
        let oldest_retained = SimTick(
            first_uncommitted
                .0
                .saturating_sub(AUTHORITY_INPUT_RETENTION_TICKS),
        );
        let latest_acceptable = SimTick(
            first_uncommitted
                .0
                .saturating_add(self.config.max_future_ticks),
        );
        InputTickWindow::new(oldest_retained, first_uncommitted, latest_acceptable).ok()
    }

    pub fn history_at(&self, seat: SeatId, tick: SimTick) -> Option<&AuthorityInputRecord> {
        self.histories[usize::from(seat.get())].get(tick)
    }

    pub fn high_water(&self, seat: SeatId) -> SeatInputHighWater {
        let history = &self.histories[usize::from(seat.get())];
        SeatInputHighWater {
            accepted: history.accepted_high_water,
            processed: history.processed_high_water,
            committed_through: self.assignments[usize::from(seat.get())]
                .and(self.last_committed_tick),
            pending_frames: history.pending_count,
            retained_frames: history.retained_count,
        }
    }

    /// Starts a new input-sequence epoch for an authenticated replacement
    /// connection. Committed history is retained for replay/acknowledgement,
    /// while uncommitted future frames from the detached generation are removed.
    pub fn begin_peer_input_epoch(
        &mut self,
        peer: PeerId,
        first_tick: SimTick,
    ) -> ProtocolResult<()> {
        peer.validate()?;
        let next_commit_tick = self
            .next_commit_tick
            .ok_or(ProtocolValidationError::InvalidTickWindow)?;
        if first_tick < next_commit_tick {
            return Err(ProtocolValidationError::InvalidTickWindow);
        }
        let mut found = false;
        for (seat_index, assignment) in self.assignments.iter().enumerate() {
            if assignment.is_some_and(|assignment| assignment.owner == SeatOwner::Peer(peer)) {
                self.histories[seat_index].begin_sequence_epoch(first_tick);
                found = true;
            }
        }
        if !found {
            return Err(ProtocolValidationError::PeerMismatch);
        }
        self.update_storage_high_water();
        Ok(())
    }

    pub fn ingest_peer_batch(
        &mut self,
        connected_peer: PeerId,
        batch: &InputBatch,
    ) -> ProtocolResult<InputIngestReport> {
        if let Err(error) = connected_peer.validate() {
            self.reject_invalid_batch(Some(connected_peer));
            return Err(error);
        }
        if let Err(error) = batch.validate_structure() {
            self.reject_invalid_batch(Some(connected_peer));
            return Err(error);
        }
        if batch.match_id != self.match_id {
            self.metrics.rejected_match_batches =
                self.metrics.rejected_match_batches.saturating_add(1);
            self.note_violation(connected_peer, 1);
            return Err(ProtocolValidationError::MatchMismatch);
        }
        if batch.peer_id != connected_peer {
            self.metrics.rejected_peer_batches =
                self.metrics.rejected_peer_batches.saturating_add(1);
            self.note_violation(connected_peer, 1);
            return Err(ProtocolValidationError::PeerMismatch);
        }

        self.prune_expired();
        let mut report = InputIngestReport::default();
        for window in batch.as_slice() {
            let Some(newest) = window.newest() else {
                // `validate_structure` above makes this unreachable.
                self.reject_invalid_batch(Some(connected_peer));
                return Err(ProtocolValidationError::EmptyInputWindow);
            };
            let fighter = match self
                .ownership
                .validate_peer_input(connected_peer, newest.seat)
            {
                Ok(fighter) => fighter,
                Err(error) => {
                    let reason = ownership_rejection(error);
                    for _ in window.as_slice() {
                        let outcome = FrameIngestOutcome::Rejected(reason);
                        self.record_outcome(outcome, None, Some(connected_peer));
                        report.record(outcome);
                    }
                    continue;
                }
            };

            // Windows are newest-first. Anchoring on the newest accepted cursor
            // lets older unseen redundancy recover only the protocol-sized tail.
            for (offset, frame) in window.as_slice().iter().enumerate() {
                let outcome = self.ingest_validated_frame(
                    *frame,
                    fighter,
                    AuthorityInputOrigin::Peer(connected_peer),
                    offset != 0,
                    Some(connected_peer),
                );
                report.record(outcome);
            }
        }
        Ok(report)
    }

    pub fn ingest_bot_frame(&mut self, frame: InputFrame) -> FrameIngestOutcome {
        self.prune_expired();
        let assignment = frame
            .seat
            .validate()
            .ok()
            .and_then(|_| self.assignments[usize::from(frame.seat.get())]);
        let outcome = match assignment {
            None => FrameIngestOutcome::Rejected(InputRejectionReason::UnownedSeat),
            Some(assignment) if assignment.owner != SeatOwner::AuthorityBot => {
                FrameIngestOutcome::Rejected(InputRejectionReason::SeatOwnedByDifferentPeer)
            }
            Some(assignment) => {
                return self.ingest_validated_frame(
                    frame,
                    assignment.fighter,
                    AuthorityInputOrigin::AuthorityBot,
                    false,
                    None,
                );
            }
        };
        self.record_outcome(outcome, None, None);
        outcome
    }

    pub fn commit_tick(&mut self, tick: SimTick) -> Result<CommittedTickInputs, CommitInputError> {
        self.commit_tick_with_overrides(tick, &[AuthoritySeatCommitOverride::Normal; MAX_SEATS])
    }

    /// Commits one deadline with explicit authority control for disconnected
    /// peer seats. Every override is validated before any history is mutated.
    pub fn commit_tick_with_overrides(
        &mut self,
        tick: SimTick,
        overrides: &[AuthoritySeatCommitOverride; MAX_SEATS],
    ) -> Result<CommittedTickInputs, CommitInputError> {
        let Some(expected) = self.next_commit_tick else {
            return Err(CommitInputError::TimelineExhausted);
        };
        if tick.0 < expected.0 {
            return Err(CommitInputError::AlreadyCommitted {
                requested: tick,
                next_expected: expected,
            });
        }
        if tick.0 > expected.0 {
            return Err(CommitInputError::TickGap {
                requested: tick,
                next_expected: expected,
            });
        }

        self.prune_expired();
        for (seat_index, assignment) in self.assignments.iter().enumerate() {
            let seat = SeatId::new(seat_index as u8)
                .expect("the fixed override array contains protocol seat indices");
            match (assignment, overrides[seat_index]) {
                (None, AuthoritySeatCommitOverride::Normal) => {}
                (None, _) => return Err(CommitInputError::OverrideForUnownedSeat(seat)),
                (Some(assignment), AuthoritySeatCommitOverride::Normal) => {
                    let _ = assignment;
                }
                (
                    Some(assignment),
                    AuthoritySeatCommitOverride::ForceNeutral
                    | AuthoritySeatCommitOverride::DisconnectedBot { .. },
                ) if assignment.owner == SeatOwner::AuthorityBot => {
                    return Err(CommitInputError::OverrideForAuthorityBot(seat));
                }
                (Some(assignment), AuthoritySeatCommitOverride::ForceNeutral) => {
                    debug_assert!(matches!(assignment.owner, SeatOwner::Peer(_)));
                }
                (
                    Some(assignment),
                    AuthoritySeatCommitOverride::DisconnectedBot { peer_id, frame },
                ) => {
                    if assignment.owner != SeatOwner::Peer(peer_id) {
                        return Err(CommitInputError::OverrideOwnerMismatch(seat));
                    }
                    if frame.tick != tick || frame.seat != seat {
                        return Err(CommitInputError::OverrideFrameMismatch(seat));
                    }
                    if frame.validate().is_err() {
                        return Err(CommitInputError::InvalidOverrideFrame(seat));
                    }
                }
            }
            if assignment.is_some() && !self.histories[seat_index].slot_available_for(tick) {
                return Err(CommitInputError::HistoryCapacity);
            }
        }

        let mut committed = CommittedTickInputs {
            tick,
            by_seat: [None; MAX_SEATS],
        };

        for seat_index in 0..MAX_SEATS {
            let Some(assignment) = self.assignments[seat_index] else {
                continue;
            };
            let record = match overrides[seat_index] {
                AuthoritySeatCommitOverride::Normal => {
                    let accepted = self.histories[seat_index].get(tick).copied();
                    if accepted.is_some_and(|record| {
                        record.status == AuthorityInputStatus::Buffered && !record.was_substituted()
                    }) {
                        self.histories[seat_index]
                            .mark_committed(tick)
                            .expect("preflighted buffered input must remain in its history slot")
                    } else {
                        let frame = substitute_frame(
                            tick,
                            assignment.seat,
                            self.histories[seat_index].last_committed_frame,
                        );
                        AuthorityInputRecord {
                            frame,
                            fighter: assignment.fighter,
                            origin: AuthorityInputOrigin::MissingSubstitute,
                            status: AuthorityInputStatus::Committed,
                        }
                    }
                }
                AuthoritySeatCommitOverride::ForceNeutral => AuthorityInputRecord {
                    frame: neutral_input_frame(tick, assignment.seat),
                    fighter: assignment.fighter,
                    origin: AuthorityInputOrigin::MissingSubstitute,
                    status: AuthorityInputStatus::Committed,
                },
                AuthoritySeatCommitOverride::DisconnectedBot { peer_id, frame } => {
                    AuthorityInputRecord {
                        frame,
                        fighter: assignment.fighter,
                        origin: AuthorityInputOrigin::DisconnectedBot(peer_id),
                        status: AuthorityInputStatus::Committed,
                    }
                }
            };

            if self.histories[seat_index]
                .get(tick)
                .is_none_or(|existing| existing.status != AuthorityInputStatus::Committed)
                && !self.histories[seat_index].insert_committed(record)
            {
                return Err(CommitInputError::HistoryCapacity);
            }
            if record.was_substituted() {
                self.metrics.substituted_frames = self.metrics.substituted_frames.saturating_add(1);
            }

            self.histories[seat_index].last_committed_frame = Some(record.frame);
            if matches!(record.origin, AuthorityInputOrigin::Peer(_)) {
                self.histories[seat_index].processed_high_water = Some(InputCursor {
                    tick: record.frame.tick,
                    sequence: record.frame.sequence,
                });
            }
            committed.by_seat[seat_index] = Some(record);
            self.metrics.committed_frames = self.metrics.committed_frames.saturating_add(1);
        }

        self.last_committed_tick = Some(tick);
        self.next_commit_tick = tick.0.checked_add(1).map(SimTick);
        self.prune_expired();
        self.update_storage_high_water();
        Ok(committed)
    }

    pub fn acknowledgement(&self) -> ProcessedInputAcknowledgement {
        let mut acknowledgement = ProcessedInputAcknowledgement {
            match_id: self.match_id,
            authority_tick: self.last_committed_tick,
            count: 0,
            seats: [SeatProcessedInputAcknowledgement::default(); MAX_SEATS],
        };
        for seat_index in 0..MAX_SEATS {
            let Some(assignment) = self.assignments[seat_index] else {
                continue;
            };
            let output_index = acknowledgement.count as usize;
            let history = &self.histories[seat_index];
            acknowledgement.seats[output_index] = SeatProcessedInputAcknowledgement {
                seat: assignment.seat,
                fighter: assignment.fighter,
                processed_through: self.last_committed_tick,
                processed_input: history.processed_high_water,
                accepted_high_water: history.accepted_high_water,
            };
            acknowledgement.count += 1;
        }
        acknowledgement
    }

    pub fn abuse_violations(&self, peer: PeerId) -> u32 {
        self.peer_abuse
            .iter()
            .find(|state| state.peer == Some(peer))
            .map_or(0, |state| state.violations)
    }

    pub fn abuse_signal(&self, peer: PeerId) -> AbuseSignal {
        let violations = self.abuse_violations(peer);
        if violations >= self.config.abuse_disconnect_threshold {
            AbuseSignal::Disconnect
        } else if violations >= self.config.abuse_warning_threshold {
            AbuseSignal::Warning
        } else {
            AbuseSignal::None
        }
    }

    /// Returns each escalation at most once for a peer.
    pub fn take_abuse_signal(&mut self, peer: PeerId) -> AbuseSignal {
        let signal = self.abuse_signal(peer);
        let Some(state) = self
            .peer_abuse
            .iter_mut()
            .find(|state| state.peer == Some(peer))
        else {
            return AbuseSignal::None;
        };
        if signal > state.emitted {
            state.emitted = signal;
            signal
        } else {
            AbuseSignal::None
        }
    }

    fn ingest_validated_frame(
        &mut self,
        frame: InputFrame,
        fighter: FighterId,
        origin: AuthorityInputOrigin,
        redundant: bool,
        abusive_peer: Option<PeerId>,
    ) -> FrameIngestOutcome {
        let result = self.try_accept_frame(frame, fighter, origin);
        let outcome = match result {
            Ok(()) => FrameIngestOutcome::Accepted { redundant },
            Err(reason) => FrameIngestOutcome::Rejected(reason),
        };
        self.record_outcome(outcome, Some(origin), abusive_peer);
        outcome
    }

    fn try_accept_frame(
        &mut self,
        frame: InputFrame,
        fighter: FighterId,
        origin: AuthorityInputOrigin,
    ) -> Result<(), InputRejectionReason> {
        frame
            .validate()
            .map_err(|_| InputRejectionReason::Invalid)?;
        let Some(tick_window) = self.tick_window() else {
            return Err(InputRejectionReason::CommittedLate);
        };
        let seat_index = usize::from(frame.seat.get());
        let history = &self.histories[seat_index];

        if let Some(existing) = history.get(frame.tick) {
            if !existing.was_substituted() && existing.frame == frame {
                return Err(InputRejectionReason::Duplicate);
            }
            if frame.tick.0 < tick_window.first_uncommitted.0
                || existing.status == AuthorityInputStatus::Committed
            {
                return Err(InputRejectionReason::CommittedLate);
            }
            return Err(InputRejectionReason::ConflictingFrame);
        }
        if frame.tick.0 < tick_window.oldest_retained.0 {
            return Err(InputRejectionReason::Stale);
        }
        if frame.tick.0 < tick_window.first_uncommitted.0 {
            return Err(InputRejectionReason::CommittedLate);
        }
        if frame.tick.0 > tick_window.latest_acceptable.0 {
            return Err(InputRejectionReason::Future);
        }
        if history.contains_received_sequence(frame.sequence) {
            return Err(InputRejectionReason::Duplicate);
        }
        validate_sequence_cursor(history.accepted_high_water, &frame)?;
        if !history.slot_available_for(frame.tick) {
            return Err(InputRejectionReason::HistoryCapacity);
        }

        let record = AuthorityInputRecord {
            frame,
            fighter,
            origin,
            status: AuthorityInputStatus::Buffered,
        };
        let history = &mut self.histories[seat_index];
        if !history.insert_buffered(record) {
            return Err(InputRejectionReason::HistoryCapacity);
        }
        if history
            .accepted_high_water
            .is_none_or(|cursor| frame.tick.0 > cursor.tick.0)
        {
            history.accepted_high_water = Some(InputCursor {
                tick: frame.tick,
                sequence: frame.sequence,
            });
        }
        Ok(())
    }

    fn record_outcome(
        &mut self,
        outcome: FrameIngestOutcome,
        origin: Option<AuthorityInputOrigin>,
        abusive_peer: Option<PeerId>,
    ) {
        match outcome {
            FrameIngestOutcome::Accepted { redundant } => {
                self.metrics.accepted_frames = self.metrics.accepted_frames.saturating_add(1);
                match origin {
                    Some(AuthorityInputOrigin::Peer(_)) => {
                        self.metrics.accepted_peer_frames =
                            self.metrics.accepted_peer_frames.saturating_add(1);
                    }
                    Some(AuthorityInputOrigin::AuthorityBot) => {
                        self.metrics.accepted_bot_frames =
                            self.metrics.accepted_bot_frames.saturating_add(1);
                    }
                    Some(AuthorityInputOrigin::DisconnectedBot(_)) => {}
                    _ => {}
                }
                if redundant {
                    self.metrics.redundant_recoveries =
                        self.metrics.redundant_recoveries.saturating_add(1);
                }
                self.update_storage_high_water();
            }
            FrameIngestOutcome::Rejected(reason) => {
                self.metrics.rejected_frames = self.metrics.rejected_frames.saturating_add(1);
                let counter = match reason {
                    InputRejectionReason::Invalid => &mut self.metrics.rejected_invalid_frames,
                    InputRejectionReason::UnownedSeat
                    | InputRejectionReason::SeatOwnedByDifferentPeer
                    | InputRejectionReason::AuthorityOwnedSeat => {
                        &mut self.metrics.rejected_unowned_frames
                    }
                    InputRejectionReason::Stale => &mut self.metrics.rejected_stale_frames,
                    InputRejectionReason::Future => &mut self.metrics.rejected_future_frames,
                    InputRejectionReason::Duplicate => &mut self.metrics.rejected_duplicate_frames,
                    InputRejectionReason::CommittedLate => {
                        &mut self.metrics.rejected_committed_late_frames
                    }
                    InputRejectionReason::SequenceOutsideWindow => {
                        self.metrics.rejected_invalid_frames =
                            self.metrics.rejected_invalid_frames.saturating_add(1);
                        &mut self.metrics.rejected_sequence_frames
                    }
                    InputRejectionReason::ConflictingFrame => {
                        self.metrics.rejected_invalid_frames =
                            self.metrics.rejected_invalid_frames.saturating_add(1);
                        &mut self.metrics.rejected_conflicting_frames
                    }
                    InputRejectionReason::HistoryCapacity => {
                        &mut self.metrics.rejected_capacity_frames
                    }
                };
                *counter = counter.saturating_add(1);
                if reason.counts_as_abuse() {
                    if let Some(peer) = abusive_peer {
                        self.note_violation(peer, 1);
                    }
                }
            }
        }
    }

    fn reject_invalid_batch(&mut self, peer: Option<PeerId>) {
        self.metrics.rejected_invalid_batches =
            self.metrics.rejected_invalid_batches.saturating_add(1);
        self.metrics.rejected_invalid_frames =
            self.metrics.rejected_invalid_frames.saturating_add(1);
        if let Some(peer) = peer {
            self.note_violation(peer, 1);
        }
    }

    fn note_violation(&mut self, peer: PeerId, count: u32) {
        if let Some(state) = self
            .peer_abuse
            .iter_mut()
            .find(|state| state.peer == Some(peer))
        {
            state.violations = state.violations.saturating_add(count);
        }
    }

    fn prune_expired(&mut self) {
        let Some(window) = self.tick_window() else {
            return;
        };
        for history in &mut self.histories {
            history.prune_before(window.oldest_retained);
        }
    }

    fn update_storage_high_water(&mut self) {
        let pending = self
            .histories
            .iter()
            .map(|history| history.pending_count)
            .sum::<u16>();
        let retained = self
            .histories
            .iter()
            .map(|history| history.retained_count)
            .sum::<u16>();
        self.metrics.maximum_pending_frames = self.metrics.maximum_pending_frames.max(pending);
        self.metrics.maximum_retained_frames = self.metrics.maximum_retained_frames.max(retained);
    }
}

pub fn neutral_input_frame(tick: SimTick, seat: SeatId) -> InputFrame {
    InputFrame {
        tick,
        seat,
        ..InputFrame::default()
    }
}

fn substitute_frame(tick: SimTick, seat: SeatId, previous: Option<InputFrame>) -> InputFrame {
    let mut frame = previous.unwrap_or_else(|| neutral_input_frame(tick, seat));
    frame.tick = tick;
    frame.seat = seat;
    frame.pressed_buttons = InputButtons::default();
    frame.released_buttons = InputButtons::default();
    frame
}

fn validate_sequence_cursor(
    high_water: Option<InputCursor>,
    frame: &InputFrame,
) -> Result<(), InputRejectionReason> {
    let Some(high_water) = high_water else {
        return Ok(());
    };
    if frame.tick.0 > high_water.tick.0 {
        let tick_delta = frame.tick.0 - high_water.tick.0;
        let expected = high_water.sequence.0.wrapping_add(tick_delta as u16);
        return (frame.sequence.0 == expected)
            .then_some(())
            .ok_or(InputRejectionReason::SequenceOutsideWindow);
    }
    if frame.tick.0 < high_water.tick.0 {
        let tick_delta = high_water.tick.0 - frame.tick.0;
        if tick_delta > MAX_INPUT_REDUNDANCY as u64 {
            return Err(InputRejectionReason::SequenceOutsideWindow);
        }
        let expected = high_water.sequence.0.wrapping_sub(tick_delta as u16);
        return (frame.sequence.0 == expected)
            .then_some(())
            .ok_or(InputRejectionReason::SequenceOutsideWindow);
    }
    Err(InputRejectionReason::Duplicate)
}

fn ownership_rejection(error: ProtocolValidationError) -> InputRejectionReason {
    match error {
        ProtocolValidationError::UnownedSeat => InputRejectionReason::UnownedSeat,
        ProtocolValidationError::SeatOwnedByDifferentPeer => {
            InputRejectionReason::SeatOwnedByDifferentPeer
        }
        ProtocolValidationError::AuthorityOwnedSeat => InputRejectionReason::AuthorityOwnedSeat,
        _ => InputRejectionReason::Invalid,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network_protocol::{QuantizedAxis, SeatInputWindow};

    fn match_id() -> MatchId {
        MatchId::new([7; 16]).unwrap()
    }

    fn peer(value: u64) -> PeerId {
        PeerId::new(value).unwrap()
    }

    fn seat(value: u8) -> SeatId {
        SeatId::new(value).unwrap()
    }

    fn fighter(value: u8) -> FighterId {
        FighterId::new(value).unwrap()
    }

    fn assignment(seat_id: u8, fighter_id: u8, owner: SeatOwner) -> SeatAssignment {
        SeatAssignment {
            seat: seat(seat_id),
            fighter: fighter(fighter_id),
            owner,
        }
    }

    fn input_frame(
        tick: u64,
        seat_id: u8,
        sequence: u16,
        movement_x: i8,
        held: u16,
        pressed: u16,
        released: u16,
    ) -> InputFrame {
        InputFrame {
            tick: SimTick(tick),
            seat: seat(seat_id),
            movement_x: QuantizedAxis::new(movement_x).unwrap(),
            movement_y: QuantizedAxis::default(),
            held_buttons: InputButtons::new(held).unwrap(),
            pressed_buttons: InputButtons::new(pressed).unwrap(),
            released_buttons: InputButtons::new(released).unwrap(),
            sequence: InputSequence(sequence),
        }
    }

    fn batch(peer_id: PeerId, windows: &[SeatInputWindow]) -> InputBatch {
        InputBatch::new(match_id(), peer_id, windows).unwrap()
    }

    fn collector(assignments: &[SeatAssignment], first_tick: u64) -> AuthorityInputCollector {
        AuthorityInputCollector::new(
            match_id(),
            SeatOwnership::from_assignments(assignments).unwrap(),
            SimTick(first_tick),
            AuthorityInputConfig::default(),
        )
        .unwrap()
    }

    #[test]
    fn enforces_multi_seat_peer_ownership_and_bot_ownership() {
        let peer_a = peer(1);
        let peer_b = peer(2);
        let mut authority = collector(
            &[
                assignment(0, 0, SeatOwner::Peer(peer_a)),
                assignment(1, 1, SeatOwner::Peer(peer_a)),
                assignment(2, 2, SeatOwner::Peer(peer_b)),
                assignment(3, 3, SeatOwner::AuthorityBot),
            ],
            0,
        );
        let seat_zero =
            SeatInputWindow::from_newest_first(&[input_frame(0, 0, 10, 1, 0, 0, 0)]).unwrap();
        let seat_one =
            SeatInputWindow::from_newest_first(&[input_frame(0, 1, 20, 2, 0, 0, 0)]).unwrap();
        let accepted = authority
            .ingest_peer_batch(peer_a, &batch(peer_a, &[seat_zero, seat_one]))
            .unwrap();
        assert_eq!(accepted.accepted, 2);

        let forged =
            SeatInputWindow::from_newest_first(&[input_frame(0, 2, 30, 3, 0, 0, 0)]).unwrap();
        let rejected = authority
            .ingest_peer_batch(peer_a, &batch(peer_a, &[forged]))
            .unwrap();
        assert_eq!(rejected.rejections.unowned, 1);

        assert_eq!(
            authority.ingest_bot_frame(input_frame(0, 3, 40, 4, 0, 0, 0)),
            FrameIngestOutcome::Accepted { redundant: false }
        );
        let committed = authority.commit_tick(SimTick(0)).unwrap();
        assert_eq!(committed.len(), 4);
        assert_eq!(
            committed.for_seat(seat(0)).unwrap().origin,
            AuthorityInputOrigin::Peer(peer_a)
        );
        assert!(committed.for_seat(seat(2)).unwrap().was_substituted());
        assert_eq!(
            committed.for_seat(seat(3)).unwrap().origin,
            AuthorityInputOrigin::AuthorityBot
        );
    }

    #[test]
    fn redundancy_recovers_unseen_older_frames() {
        let owner = peer(1);
        let mut authority = collector(&[assignment(0, 0, SeatOwner::Peer(owner))], 0);
        let frames = [
            input_frame(4, 0, 104, 4, 0, 0, 0),
            input_frame(3, 0, 103, 3, 0, InputButtons::LIGHT, 0),
            input_frame(2, 0, 102, 2, InputButtons::LIGHT, 0, 0),
        ];
        let window = SeatInputWindow::from_newest_first(&frames).unwrap();
        let report = authority
            .ingest_peer_batch(owner, &batch(owner, &[window]))
            .unwrap();
        assert_eq!(report.accepted, 3);
        assert_eq!(report.redundant_recoveries, 2);
        assert_eq!(authority.metrics().redundant_recoveries, 2);

        authority.commit_tick(SimTick(0)).unwrap();
        authority.commit_tick(SimTick(1)).unwrap();
        let recovered = authority.commit_tick(SimTick(2)).unwrap();
        assert_eq!(
            recovered.for_seat(seat(0)).unwrap().frame.movement_x.get(),
            2
        );
        assert!(!recovered.for_seat(seat(0)).unwrap().was_substituted());
    }

    #[test]
    fn duplicate_detection_and_sequence_high_water_handle_wrap() {
        let owner = peer(1);
        let mut authority = collector(&[assignment(0, 0, SeatOwner::Peer(owner))], 100);
        let before_wrap =
            SeatInputWindow::from_newest_first(&[input_frame(100, 0, u16::MAX, 0, 0, 0, 0)])
                .unwrap();
        assert_eq!(
            authority
                .ingest_peer_batch(owner, &batch(owner, &[before_wrap]))
                .unwrap()
                .accepted,
            1
        );
        assert_eq!(
            authority
                .ingest_peer_batch(owner, &batch(owner, &[before_wrap]))
                .unwrap()
                .rejections
                .duplicate,
            1
        );

        let after_wrap =
            SeatInputWindow::from_newest_first(&[input_frame(101, 0, 0, 0, 0, 0, 0)]).unwrap();
        assert_eq!(
            authority
                .ingest_peer_batch(owner, &batch(owner, &[after_wrap]))
                .unwrap()
                .accepted,
            1
        );
        assert_eq!(
            authority.high_water(seat(0)).accepted.unwrap().sequence.0,
            0
        );

        let reused_sequence =
            SeatInputWindow::from_newest_first(&[input_frame(102, 0, u16::MAX, 0, 0, 0, 0)])
                .unwrap();
        let report = authority
            .ingest_peer_batch(owner, &batch(owner, &[reused_sequence]))
            .unwrap();
        assert_eq!(report.rejections.duplicate, 1);
    }

    #[test]
    fn authenticated_reconnect_starts_new_sequence_epoch_without_losing_committed_history() {
        let owner = peer(1);
        let mut authority = collector(&[assignment(0, 0, SeatOwner::Peer(owner))], 0);
        let old_generation = SeatInputWindow::from_newest_first(&[
            input_frame(2, 0, 102, 2, 0, 0, 0),
            input_frame(1, 0, 101, 1, 0, 0, 0),
            input_frame(0, 0, 100, 0, 0, 0, 0),
        ])
        .unwrap();
        assert_eq!(
            authority
                .ingest_peer_batch(owner, &batch(owner, &[old_generation]))
                .unwrap()
                .accepted,
            3
        );
        authority.commit_tick(SimTick(0)).unwrap();

        authority.begin_peer_input_epoch(owner, SimTick(1)).unwrap();
        assert_eq!(
            authority
                .history_at(seat(0), SimTick(0))
                .unwrap()
                .frame
                .sequence,
            InputSequence(100)
        );
        assert!(authority.history_at(seat(0), SimTick(1)).is_none());
        assert!(authority.history_at(seat(0), SimTick(2)).is_none());

        let replacement_generation = SeatInputWindow::from_newest_first(&[
            input_frame(2, 0, 1, 12, 0, 0, 0),
            input_frame(1, 0, 0, 11, 0, 0, 0),
        ])
        .unwrap();
        let report = authority
            .ingest_peer_batch(owner, &batch(owner, &[replacement_generation]))
            .unwrap();
        assert_eq!(report.accepted, 2);
        assert_eq!(report.rejected, 0);
        assert_eq!(
            authority
                .commit_tick(SimTick(1))
                .unwrap()
                .for_seat(seat(0))
                .unwrap()
                .frame
                .sequence,
            InputSequence(0)
        );
        assert_eq!(
            authority
                .commit_tick(SimTick(2))
                .unwrap()
                .for_seat(seat(0))
                .unwrap()
                .frame
                .sequence,
            InputSequence(1)
        );
    }

    #[test]
    fn missing_input_repeats_continuous_state_without_edges() {
        let owner = peer(1);
        let mut authority = collector(
            &[
                assignment(0, 0, SeatOwner::Peer(owner)),
                assignment(1, 1, SeatOwner::AuthorityBot),
            ],
            0,
        );
        let received = input_frame(
            0,
            0,
            10,
            73,
            InputButtons::LIGHT | InputButtons::GUARD,
            InputButtons::LIGHT,
            InputButtons::JUMP,
        );
        let window = SeatInputWindow::from_newest_first(&[received]).unwrap();
        authority
            .ingest_peer_batch(owner, &batch(owner, &[window]))
            .unwrap();
        let first = authority.commit_tick(SimTick(0)).unwrap();
        assert_eq!(
            first
                .for_seat(seat(0))
                .unwrap()
                .frame
                .pressed_buttons
                .bits(),
            InputButtons::LIGHT
        );

        let repeated = authority.commit_tick(SimTick(1)).unwrap();
        let repeated = repeated.for_seat(seat(0)).unwrap();
        assert_eq!(repeated.frame.movement_x.get(), 73);
        assert_eq!(
            repeated.frame.held_buttons.bits(),
            InputButtons::LIGHT | InputButtons::GUARD
        );
        assert_eq!(repeated.frame.pressed_buttons.bits(), 0);
        assert_eq!(repeated.frame.released_buttons.bits(), 0);
        assert!(repeated.was_substituted());

        let neutral = authority.history_at(seat(1), SimTick(0)).unwrap();
        assert_eq!(neutral.frame.movement_x.get(), 0);
        assert_eq!(neutral.frame.held_buttons.bits(), 0);
        assert!(neutral.was_substituted());
    }

    #[test]
    fn committed_late_input_never_rewrites_history() {
        let owner = peer(1);
        let mut authority = collector(&[assignment(0, 0, SeatOwner::Peer(owner))], 0);
        let original = authority.commit_tick(SimTick(0)).unwrap();
        assert!(original.for_seat(seat(0)).unwrap().was_substituted());

        let late = SeatInputWindow::from_newest_first(&[input_frame(
            0,
            0,
            10,
            127,
            InputButtons::HEAVY,
            InputButtons::HEAVY,
            0,
        )])
        .unwrap();
        let report = authority
            .ingest_peer_batch(owner, &batch(owner, &[late]))
            .unwrap();
        assert_eq!(report.rejections.committed_late, 1);
        assert_eq!(authority.metrics().rejected_committed_late_frames, 1);
        let retained = authority.history_at(seat(0), SimTick(0)).unwrap();
        assert!(retained.was_substituted());
        assert_eq!(retained.frame.movement_x.get(), 0);
        assert!(matches!(
            authority.commit_tick(SimTick(0)),
            Err(CommitInputError::AlreadyCommitted { .. })
        ));
    }

    #[test]
    fn repeated_invalid_input_emits_rate_limited_abuse_escalation() {
        let owner = peer(1);
        let config = AuthorityInputConfig {
            max_future_ticks: 2,
            abuse_warning_threshold: 2,
            abuse_disconnect_threshold: 3,
        };
        let mut authority = AuthorityInputCollector::new(
            match_id(),
            SeatOwnership::from_assignments(&[assignment(0, 0, SeatOwner::Peer(owner))]).unwrap(),
            SimTick(0),
            config,
        )
        .unwrap();

        for sequence in 0..2 {
            let future = SeatInputWindow::from_newest_first(&[input_frame(
                100 + u64::from(sequence),
                0,
                sequence,
                0,
                0,
                0,
                0,
            )])
            .unwrap();
            authority
                .ingest_peer_batch(owner, &batch(owner, &[future]))
                .unwrap();
        }
        assert_eq!(authority.abuse_signal(owner), AbuseSignal::Warning);
        assert_eq!(authority.take_abuse_signal(owner), AbuseSignal::Warning);
        assert_eq!(authority.take_abuse_signal(owner), AbuseSignal::None);

        let future =
            SeatInputWindow::from_newest_first(&[input_frame(102, 0, 2, 0, 0, 0, 0)]).unwrap();
        authority
            .ingest_peer_batch(owner, &batch(owner, &[future]))
            .unwrap();
        assert_eq!(authority.abuse_signal(owner), AbuseSignal::Disconnect);
        assert_eq!(authority.take_abuse_signal(owner), AbuseSignal::Disconnect);
    }

    #[test]
    fn histories_and_pending_input_remain_strictly_bounded() {
        let owner = peer(1);
        let mut authority = collector(&[assignment(0, 0, SeatOwner::Peer(owner))], 0);
        for tick in 0..300u64 {
            let window = SeatInputWindow::from_newest_first(&[input_frame(
                tick,
                0,
                tick as u16,
                0,
                0,
                0,
                0,
            )])
            .unwrap();
            assert_eq!(
                authority
                    .ingest_peer_batch(owner, &batch(owner, &[window]))
                    .unwrap()
                    .accepted,
                1
            );
            authority.commit_tick(SimTick(tick)).unwrap();
            let high_water = authority.high_water(seat(0));
            assert!(usize::from(high_water.retained_frames) <= AUTHORITY_INPUT_HISTORY_CAPACITY);
            assert_eq!(high_water.pending_frames, 0);
        }
        assert!(authority.history_at(seat(0), SimTick(0)).is_none());
        assert!(authority.history_at(seat(0), SimTick(299)).is_some());
        assert!(
            usize::from(authority.metrics().maximum_retained_frames)
                <= AUTHORITY_INPUT_HISTORY_CAPACITY * MAX_SEATS
        );
        assert!(
            usize::from(authority.metrics().maximum_pending_frames)
                <= AUTHORITY_INPUT_HISTORY_CAPACITY * MAX_SEATS
        );
    }

    #[test]
    fn acknowledgement_separates_received_and_processed_high_water() {
        let owner = peer(1);
        let mut authority = collector(&[assignment(0, 0, SeatOwner::Peer(owner))], 0);
        let future =
            SeatInputWindow::from_newest_first(&[input_frame(2, 0, 12, 0, 0, 0, 0)]).unwrap();
        authority
            .ingest_peer_batch(owner, &batch(owner, &[future]))
            .unwrap();

        authority.commit_tick(SimTick(0)).unwrap();
        let acknowledgement = authority.acknowledgement();
        let seat_ack = acknowledgement.for_seat(seat(0)).unwrap();
        assert_eq!(seat_ack.processed_through, Some(SimTick(0)));
        assert_eq!(seat_ack.processed_input, None);
        assert_eq!(seat_ack.accepted_high_water.unwrap().tick, SimTick(2));

        authority.commit_tick(SimTick(1)).unwrap();
        authority.commit_tick(SimTick(2)).unwrap();
        let acknowledgement = authority.acknowledgement();
        let seat_ack = acknowledgement.for_seat(seat(0)).unwrap();
        assert_eq!(
            seat_ack.processed_input.unwrap().sequence,
            InputSequence(12)
        );
    }

    #[test]
    fn disconnect_neutral_override_replaces_buffered_input_without_pending_growth() {
        let owner = peer(1);
        let mut authority = collector(&[assignment(0, 0, SeatOwner::Peer(owner))], 0);
        let attack = input_frame(0, 0, 10, 90, InputButtons::HEAVY, InputButtons::HEAVY, 0);
        let window = SeatInputWindow::from_newest_first(&[attack]).unwrap();
        authority
            .ingest_peer_batch(owner, &batch(owner, &[window]))
            .unwrap();
        assert_eq!(authority.high_water(seat(0)).pending_frames, 1);

        let mut overrides = [AuthoritySeatCommitOverride::Normal; MAX_SEATS];
        overrides[0] = AuthoritySeatCommitOverride::ForceNeutral;
        let committed = authority
            .commit_tick_with_overrides(SimTick(0), &overrides)
            .unwrap();
        let record = committed.for_seat(seat(0)).unwrap();
        assert_eq!(record.frame, neutral_input_frame(SimTick(0), seat(0)));
        assert_eq!(record.origin, AuthorityInputOrigin::MissingSubstitute);
        assert_eq!(authority.high_water(seat(0)).pending_frames, 0);
        assert_eq!(
            authority
                .acknowledgement()
                .for_seat(seat(0))
                .unwrap()
                .processed_input,
            None
        );
    }

    #[test]
    fn disconnect_bot_override_is_canonical_on_a_peer_owned_seat() {
        let owner = peer(1);
        let mut authority = collector(&[assignment(0, 0, SeatOwner::Peer(owner))], 5);
        let bot = input_frame(5, 0, 77, -45, InputButtons::LIGHT, 0, 0);
        let mut overrides = [AuthoritySeatCommitOverride::Normal; MAX_SEATS];
        overrides[0] = AuthoritySeatCommitOverride::DisconnectedBot {
            peer_id: owner,
            frame: bot,
        };
        let committed = authority
            .commit_tick_with_overrides(SimTick(5), &overrides)
            .unwrap();
        let record = committed.for_seat(seat(0)).unwrap();
        assert_eq!(record.frame, bot);
        assert_eq!(record.origin, AuthorityInputOrigin::DisconnectedBot(owner));
        assert!(record.was_substituted());
        assert_eq!(
            authority
                .acknowledgement()
                .for_seat(seat(0))
                .unwrap()
                .processed_input,
            None
        );
    }

    #[test]
    fn invalid_disconnect_override_fails_before_committing_any_seat() {
        let owner = peer(1);
        let other = peer(2);
        let mut authority = collector(
            &[
                assignment(0, 0, SeatOwner::Peer(owner)),
                assignment(1, 1, SeatOwner::AuthorityBot),
            ],
            0,
        );
        let mut overrides = [AuthoritySeatCommitOverride::Normal; MAX_SEATS];
        overrides[0] = AuthoritySeatCommitOverride::DisconnectedBot {
            peer_id: other,
            frame: input_frame(0, 0, 0, 0, 0, 0, 0),
        };
        assert_eq!(
            authority.commit_tick_with_overrides(SimTick(0), &overrides),
            Err(CommitInputError::OverrideOwnerMismatch(seat(0)))
        );
        assert_eq!(authority.next_commit_tick(), Some(SimTick(0)));
        assert!(authority.history_at(seat(0), SimTick(0)).is_none());

        overrides[0] = AuthoritySeatCommitOverride::Normal;
        overrides[1] = AuthoritySeatCommitOverride::ForceNeutral;
        assert_eq!(
            authority.commit_tick_with_overrides(SimTick(0), &overrides),
            Err(CommitInputError::OverrideForAuthorityBot(seat(1)))
        );
        assert_eq!(authority.next_commit_tick(), Some(SimTick(0)));
    }

    #[test]
    fn configuration_reserves_capacity_for_retained_and_future_ticks() {
        let invalid = AuthorityInputConfig {
            max_future_ticks: MAX_CONFIGURED_FUTURE_TICKS + 1,
            ..AuthorityInputConfig::default()
        };
        assert_eq!(
            invalid.validate(),
            Err(ProtocolValidationError::InvalidTickWindow)
        );
        let invalid = AuthorityInputConfig {
            abuse_warning_threshold: 3,
            abuse_disconnect_threshold: 2,
            ..AuthorityInputConfig::default()
        };
        assert_eq!(
            invalid.validate(),
            Err(ProtocolValidationError::InvalidTickWindow)
        );
    }
}
