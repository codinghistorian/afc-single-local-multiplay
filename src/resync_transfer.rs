//! Bounded production assembly for canonical resync snapshots.
//!
//! The transport owns reliability and packet scheduling. This module owns the
//! transfer contract: an authority prepares one bounded canonical byte buffer and
//! exposes chunks lazily, while a client accepts chunks in any order without an
//! unbounded queue. Invalid or conflicting traffic never overwrites accepted data.

use std::error::Error;
use std::fmt;
use std::iter::FusedIterator;

use crate::network_protocol::{
    CommittedSeatInputWindow, MAX_RESYNC_CHUNK_BYTES, MAX_RESYNC_CHUNKS,
    MAX_RESYNC_INPUT_TAIL_TICKS, MAX_RESYNC_SNAPSHOT_BYTES, MatchId, PeerId,
    ProtocolValidationError, ResyncApplied, ResyncBegin, ResyncChunk, ResyncChunkPayload,
    ResyncInputTail, ResyncReason, ResyncRequest, StateHash, TransferId,
};
use crate::simulation::SimTick;
use crate::snapshot::{CanonicalSnapshot, SnapshotError, hash_canonical_bytes};

/// Five seconds at the canonical 60 Hz network/simulation clock.
pub const DEFAULT_RESYNC_TIMEOUT_TICKS: u64 = 5 * 60;
/// Chunks may legally outrun `ResyncBegin` because Control and Resync are
/// independent reliable channels.  Unknown-transfer staging is deliberately
/// shorter lived than a full transfer and never grows beyond the snapshot cap.
pub const PRE_BEGIN_CHUNK_TIMEOUT_TICKS: u64 = 60;
pub const MAX_PRE_BEGIN_TRANSFERS: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResyncTransferError {
    Protocol(ProtocolValidationError),
    Snapshot(SnapshotError),
    InvalidTimeout,
    RequestMatchMismatch,
    RequestAheadOfSnapshot {
        confirmed: SimTick,
        snapshot: SimTick,
    },
    SnapshotTooLarge {
        bytes: usize,
        max: usize,
    },
    SnapshotMatchMismatch,
    ExpectedMatchMismatch,
    UnexpectedChunk {
        transfer_id: TransferId,
    },
    ConflictingBegin {
        transfer_id: TransferId,
    },
    StaleReplacement {
        active_tick: SimTick,
        offered_tick: SimTick,
    },
    ConflictingChunk {
        transfer_id: TransferId,
        chunk_index: u16,
    },
    ConflictingInputTail {
        transfer_id: TransferId,
    },
    MissingInputTail,
    IncompleteTransfer,
    SnapshotTickMismatch {
        expected: SimTick,
        actual: SimTick,
    },
    SnapshotHashMismatch {
        expected: StateHash,
        actual: StateHash,
    },
    AppliedMismatch,
    ClockRegressed {
        previous: SimTick,
        now: SimTick,
    },
}

impl fmt::Display for ResyncTransferError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Protocol(error) => write!(formatter, "invalid resync protocol metadata: {error}"),
            Self::Snapshot(error) => {
                write!(formatter, "invalid canonical resync snapshot: {error}")
            }
            Self::InvalidTimeout => write!(formatter, "resync timeout must be at least one tick"),
            Self::RequestMatchMismatch => {
                write!(
                    formatter,
                    "resync request does not match the snapshot match"
                )
            }
            Self::RequestAheadOfSnapshot {
                confirmed,
                snapshot,
            } => write!(
                formatter,
                "resync request confirmed tick {} is ahead of snapshot tick {}",
                confirmed.get(),
                snapshot.get()
            ),
            Self::SnapshotTooLarge { bytes, max } => {
                write!(
                    formatter,
                    "resync snapshot has {bytes} bytes; maximum is {max}"
                )
            }
            Self::SnapshotMatchMismatch => {
                write!(
                    formatter,
                    "canonical snapshot match ID differs from transfer metadata"
                )
            }
            Self::ExpectedMatchMismatch => {
                write!(formatter, "resync message belongs to a different match")
            }
            Self::UnexpectedChunk { transfer_id } => write!(
                formatter,
                "received chunk for transfer {} without an active resync",
                transfer_id.get()
            ),
            Self::ConflictingBegin { transfer_id } => write!(
                formatter,
                "conflicting begin metadata for transfer {}",
                transfer_id.get()
            ),
            Self::StaleReplacement {
                active_tick,
                offered_tick,
            } => write!(
                formatter,
                "replacement snapshot tick {} is older than active tick {}",
                offered_tick.get(),
                active_tick.get()
            ),
            Self::ConflictingChunk {
                transfer_id,
                chunk_index,
            } => write!(
                formatter,
                "chunk {chunk_index} conflicts with accepted data for transfer {}",
                transfer_id.get()
            ),
            Self::ConflictingInputTail { transfer_id } => write!(
                formatter,
                "input tail conflicts with accepted data for transfer {}",
                transfer_id.get()
            ),
            Self::MissingInputTail => {
                write!(
                    formatter,
                    "resync snapshot completed without its input tail"
                )
            }
            Self::IncompleteTransfer => {
                write!(
                    formatter,
                    "resync completion did not cover every declared byte"
                )
            }
            Self::SnapshotTickMismatch { expected, actual } => write!(
                formatter,
                "resync snapshot tick {} differs from declared tick {}",
                actual.get(),
                expected.get()
            ),
            Self::SnapshotHashMismatch { expected, actual } => write!(
                formatter,
                "resync snapshot hash {:#018x} differs from declared hash {:#018x}",
                actual.0, expected.0
            ),
            Self::AppliedMismatch => {
                write!(
                    formatter,
                    "resync applied acknowledgement metadata does not match"
                )
            }
            Self::ClockRegressed { previous, now } => write!(
                formatter,
                "resync clock regressed from tick {} to {}",
                previous.get(),
                now.get()
            ),
        }
    }
}

impl Error for ResyncTransferError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Protocol(error) => Some(error),
            Self::Snapshot(error) => Some(error),
            _ => None,
        }
    }
}

impl From<ProtocolValidationError> for ResyncTransferError {
    fn from(value: ProtocolValidationError) -> Self {
        Self::Protocol(value)
    }
}

impl From<SnapshotError> for ResyncTransferError {
    fn from(value: SnapshotError) -> Self {
        Self::Snapshot(value)
    }
}

pub type ResyncTransferResult<T> = Result<T, ResyncTransferError>;

/// Non-canonical operational counters. All counters saturate instead of wrapping.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResyncTransferMetrics {
    pub authority_transfers_prepared: u64,
    pub authority_snapshot_bytes_prepared: u64,
    pub chunks_emitted: u64,
    pub chunk_bytes_emitted: u64,
    pub applied_acknowledgements: u64,
    pub input_tails_emitted: u64,
    pub begins_accepted: u64,
    pub duplicate_begins: u64,
    pub superseded_transfers: u64,
    pub chunks_accepted: u64,
    pub chunk_bytes_accepted: u64,
    pub duplicate_chunks: u64,
    pub input_tails_accepted: u64,
    pub duplicate_input_tails: u64,
    pub conflicting_messages: u64,
    pub rejected_messages: u64,
    pub completed_transfers: u64,
    pub verification_failures: u64,
    pub timed_out_transfers: u64,
    pub manual_resets: u64,
    pub pre_begin_chunks_staged: u64,
    pub pre_begin_bytes_staged: u64,
    pub pre_begin_duplicate_chunks: u64,
    pub pre_begin_conflicting_chunks: u64,
    pub pre_begin_transfers_evicted: u64,
    pub pre_begin_chunks_evicted: u64,
    pub pre_begin_bytes_evicted: u64,
    pub pre_begin_transfers_expired: u64,
    pub pre_begin_chunks_expired: u64,
    pub pre_begin_bytes_expired: u64,
    pub pre_begin_input_tails_staged: u64,
    pub pre_begin_input_tails_expired: u64,
    pub pre_begin_input_tails_evicted: u64,
}

fn increment(counter: &mut u64) {
    *counter = counter.saturating_add(1);
}

fn add(counter: &mut u64, value: usize) {
    *counter = counter.saturating_add(value as u64);
}

/// One authority-side canonical snapshot transfer.
///
/// Only the bounded encoded snapshot is retained. [`chunks`](Self::chunks) yields
/// fixed-capacity protocol messages directly from that buffer and never creates a
/// second queue of chunks.
pub struct AuthorityResyncTransfer {
    request: ResyncRequest,
    begin: ResyncBegin,
    encoded_snapshot: Vec<u8>,
    input_tail: ResyncInputTail,
    metrics: ResyncTransferMetrics,
}

impl AuthorityResyncTransfer {
    pub fn from_snapshot(
        request: ResyncRequest,
        transfer_id: TransferId,
        snapshot: &CanonicalSnapshot,
        input_windows: &[CommittedSeatInputWindow],
    ) -> ResyncTransferResult<Self> {
        request.validate()?;
        transfer_id.validate()?;
        if request.match_id.as_bytes() != &snapshot.header.match_id {
            return Err(ResyncTransferError::RequestMatchMismatch);
        }
        if request.last_confirmed_tick > snapshot.header.tick {
            return Err(ResyncTransferError::RequestAheadOfSnapshot {
                confirmed: request.last_confirmed_tick,
                snapshot: snapshot.header.tick,
            });
        }

        let encoded_snapshot = snapshot.encode()?;
        if encoded_snapshot.len() > MAX_RESYNC_SNAPSHOT_BYTES {
            return Err(ResyncTransferError::SnapshotTooLarge {
                bytes: encoded_snapshot.len(),
                max: MAX_RESYNC_SNAPSHOT_BYTES,
            });
        }
        let chunk_count = encoded_snapshot.len().div_ceil(MAX_RESYNC_CHUNK_BYTES);
        debug_assert!((1..=MAX_RESYNC_CHUNKS).contains(&chunk_count));
        let first = input_windows
            .first()
            .ok_or(ProtocolValidationError::EmptyInputBatch)?;
        first.validate()?;
        if first.len() > MAX_RESYNC_INPUT_TAIL_TICKS {
            return Err(ProtocolValidationError::InputWindowTooLarge.into());
        }
        let recent_input_end = first
            .newest()
            .ok_or(ProtocolValidationError::EmptyInputWindow)?
            .frame
            .tick;
        let recent_input_start = first
            .as_slice()
            .last()
            .ok_or(ProtocolValidationError::EmptyInputWindow)?
            .frame
            .tick;
        let begin = ResyncBegin {
            match_id: request.match_id,
            transfer_id,
            snapshot_tick: snapshot.header.tick,
            snapshot_hash: StateHash(hash_canonical_bytes(&encoded_snapshot)),
            snapshot_bytes: encoded_snapshot.len() as u32,
            chunk_count: chunk_count as u16,
            recent_input_start,
            recent_input_end,
        };
        begin.validate()?;
        let input_tail = ResyncInputTail::new(&begin, input_windows)?;

        let mut metrics = ResyncTransferMetrics::default();
        metrics.authority_transfers_prepared = 1;
        metrics.authority_snapshot_bytes_prepared = encoded_snapshot.len() as u64;
        Ok(Self {
            request,
            begin,
            encoded_snapshot,
            input_tail,
            metrics,
        })
    }

    pub const fn request(&self) -> ResyncRequest {
        self.request
    }

    pub const fn begin(&self) -> ResyncBegin {
        self.begin
    }

    pub fn snapshot_bytes(&self) -> usize {
        self.encoded_snapshot.len()
    }

    pub fn input_tail(&mut self) -> ResyncInputTail {
        increment(&mut self.metrics.input_tails_emitted);
        self.input_tail
    }

    pub const fn metrics(&self) -> &ResyncTransferMetrics {
        &self.metrics
    }

    pub fn chunks(&mut self) -> AuthorityResyncChunkIter<'_> {
        AuthorityResyncChunkIter {
            begin: self.begin,
            encoded_snapshot: &self.encoded_snapshot,
            next_index: 0,
            metrics: &mut self.metrics,
        }
    }

    /// Starts a bounded iterator at `chunk_index`, useful for explicit retransmit.
    pub fn chunks_from(
        &mut self,
        chunk_index: u16,
    ) -> ResyncTransferResult<AuthorityResyncChunkIter<'_>> {
        if chunk_index > self.begin.chunk_count {
            return Err(ProtocolValidationError::InvalidChunkIndex.into());
        }
        Ok(AuthorityResyncChunkIter {
            begin: self.begin,
            encoded_snapshot: &self.encoded_snapshot,
            next_index: chunk_index,
            metrics: &mut self.metrics,
        })
    }

    pub fn validate_applied(&mut self, applied: &ResyncApplied) -> ResyncTransferResult<()> {
        if let Err(error) = applied.validate() {
            increment(&mut self.metrics.rejected_messages);
            return Err(error.into());
        }
        if applied.match_id != self.begin.match_id
            || applied.transfer_id != self.begin.transfer_id
            || applied.peer_id != self.request.peer_id
            || applied.snapshot_tick != self.begin.snapshot_tick
            || applied.snapshot_hash != self.begin.snapshot_hash
        {
            increment(&mut self.metrics.rejected_messages);
            return Err(ResyncTransferError::AppliedMismatch);
        }
        increment(&mut self.metrics.applied_acknowledgements);
        Ok(())
    }
}

pub struct AuthorityResyncChunkIter<'a> {
    begin: ResyncBegin,
    encoded_snapshot: &'a [u8],
    next_index: u16,
    metrics: &'a mut ResyncTransferMetrics,
}

impl Iterator for AuthorityResyncChunkIter<'_> {
    type Item = ResyncChunk;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next_index >= self.begin.chunk_count {
            return None;
        }
        let chunk_index = self.next_index;
        self.next_index += 1;
        let offset = usize::from(chunk_index) * MAX_RESYNC_CHUNK_BYTES;
        let end = (offset + MAX_RESYNC_CHUNK_BYTES).min(self.encoded_snapshot.len());
        let bytes = &self.encoded_snapshot[offset..end];
        let (payload, payload_len) = ResyncChunkPayload::from_bytes(bytes)
            .expect("authority chunks are non-empty slices within the protocol cap");
        increment(&mut self.metrics.chunks_emitted);
        add(&mut self.metrics.chunk_bytes_emitted, bytes.len());
        Some(ResyncChunk {
            match_id: self.begin.match_id,
            transfer_id: self.begin.transfer_id,
            snapshot_tick: self.begin.snapshot_tick,
            snapshot_hash: self.begin.snapshot_hash,
            chunk_index,
            chunk_count: self.begin.chunk_count,
            payload_len,
            payload,
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.len();
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for AuthorityResyncChunkIter<'_> {
    fn len(&self) -> usize {
        usize::from(self.begin.chunk_count - self.next_index)
    }
}

impl FusedIterator for AuthorityResyncChunkIter<'_> {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResyncBeginOutcome {
    Started,
    Duplicate,
    Superseded { previous_transfer_id: TransferId },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResyncTransferProgress {
    pub begin: ResyncBegin,
    pub received_chunks: u16,
    pub received_bytes: u32,
    pub received_input_tail: bool,
    pub last_progress_tick: SimTick,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResetResyncTransfer {
    pub begin: ResyncBegin,
    pub received_chunks: u16,
    pub received_bytes: u32,
    pub received_input_tail: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletedResyncTransfer {
    pub snapshot: CanonicalSnapshot,
    pub input_tail: ResyncInputTail,
    pub applied: ResyncApplied,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResyncChunkOutcome {
    Accepted(ResyncTransferProgress),
    Duplicate(ResyncTransferProgress),
    StagedBeforeBegin(PreBeginResyncProgress),
    Complete(CompletedResyncTransfer),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResyncInputTailOutcome {
    Accepted(ResyncTransferProgress),
    Duplicate(ResyncTransferProgress),
    StagedBeforeBegin(PreBeginResyncProgress),
    Complete(CompletedResyncTransfer),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreBeginResyncProgress {
    pub transfer_id: TransferId,
    pub staged_chunks: u16,
    pub staged_bytes: u32,
    pub staged_input_tail: bool,
    pub oldest_staged_tick: SimTick,
}

struct ActiveResyncTransfer {
    begin: ResyncBegin,
    bytes: Vec<u8>,
    received: [bool; MAX_RESYNC_CHUNKS],
    received_chunks: u16,
    received_bytes: usize,
    input_tail: Option<ResyncInputTail>,
    last_progress_tick: SimTick,
}

impl ActiveResyncTransfer {
    fn new(begin: ResyncBegin, now: SimTick) -> Self {
        Self {
            begin,
            bytes: vec![0; begin.snapshot_bytes as usize],
            received: [false; MAX_RESYNC_CHUNKS],
            received_chunks: 0,
            received_bytes: 0,
            input_tail: None,
            last_progress_tick: now,
        }
    }

    fn progress(&self) -> ResyncTransferProgress {
        ResyncTransferProgress {
            begin: self.begin,
            received_chunks: self.received_chunks,
            received_bytes: self.received_bytes as u32,
            received_input_tail: self.input_tail.is_some(),
            last_progress_tick: self.last_progress_tick,
        }
    }

    fn reset_metadata(&self) -> ResetResyncTransfer {
        ResetResyncTransfer {
            begin: self.begin,
            received_chunks: self.received_chunks,
            received_bytes: self.received_bytes as u32,
            received_input_tail: self.input_tail.is_some(),
        }
    }
}

#[derive(Clone, Copy)]
struct StagedPreBeginChunk {
    chunk: ResyncChunk,
    staged_tick: SimTick,
}

#[derive(Clone, Copy)]
struct StagedPreBeginInputTail {
    tail: ResyncInputTail,
    staged_tick: SimTick,
}

/// Client-side bounded out-of-order assembler for one active transfer.
pub struct ClientResyncAssembler {
    match_id: MatchId,
    peer_id: PeerId,
    timeout_ticks: u64,
    active: Option<ActiveResyncTransfer>,
    pre_begin: Box<[Option<StagedPreBeginChunk>; MAX_RESYNC_CHUNKS]>,
    pre_begin_input_tails: [Option<StagedPreBeginInputTail>; MAX_PRE_BEGIN_TRANSFERS],
    pre_begin_len: usize,
    pre_begin_bytes: usize,
    last_observed_tick: SimTick,
    metrics: ResyncTransferMetrics,
}

impl ClientResyncAssembler {
    pub fn new(
        match_id: MatchId,
        peer_id: PeerId,
        timeout_ticks: u64,
    ) -> ResyncTransferResult<Self> {
        match_id.validate()?;
        peer_id.validate()?;
        if timeout_ticks == 0 {
            return Err(ResyncTransferError::InvalidTimeout);
        }
        Ok(Self {
            match_id,
            peer_id,
            timeout_ticks,
            active: None,
            pre_begin: Box::new([None; MAX_RESYNC_CHUNKS]),
            pre_begin_input_tails: [None; MAX_PRE_BEGIN_TRANSFERS],
            pre_begin_len: 0,
            pre_begin_bytes: 0,
            last_observed_tick: SimTick::ZERO,
            metrics: ResyncTransferMetrics::default(),
        })
    }

    pub fn with_default_timeout(match_id: MatchId, peer_id: PeerId) -> ResyncTransferResult<Self> {
        Self::new(match_id, peer_id, DEFAULT_RESYNC_TIMEOUT_TICKS)
    }

    pub const fn metrics(&self) -> &ResyncTransferMetrics {
        &self.metrics
    }

    pub fn active_progress(&self) -> Option<ResyncTransferProgress> {
        self.active.as_ref().map(ActiveResyncTransfer::progress)
    }

    pub const fn staged_pre_begin_chunks(&self) -> usize {
        self.pre_begin_len
    }

    pub const fn staged_pre_begin_bytes(&self) -> usize {
        self.pre_begin_bytes
    }

    pub fn staged_pre_begin_input_tails(&self) -> usize {
        self.pre_begin_input_tails.iter().flatten().count()
    }

    pub const fn timeout_ticks(&self) -> u64 {
        self.timeout_ticks
    }

    pub fn make_request(
        &self,
        reason: ResyncReason,
        last_confirmed_tick: SimTick,
        last_confirmed_hash: StateHash,
    ) -> ResyncRequest {
        ResyncRequest {
            match_id: self.match_id,
            peer_id: self.peer_id,
            reason,
            last_confirmed_tick,
            last_confirmed_hash,
        }
    }

    pub fn accept_begin(
        &mut self,
        begin: ResyncBegin,
        now: SimTick,
    ) -> ResyncTransferResult<ResyncBeginOutcome> {
        self.observe_clock(now)?;
        self.expire_pre_begin(now);
        if let Err(error) = begin.validate() {
            increment(&mut self.metrics.rejected_messages);
            return Err(error.into());
        }
        if begin.match_id != self.match_id {
            increment(&mut self.metrics.rejected_messages);
            return Err(ResyncTransferError::ExpectedMatchMismatch);
        }

        if let Some(active) = &self.active {
            validate_clock(active.last_progress_tick, now)?;
            if begin.transfer_id == active.begin.transfer_id {
                if begin == active.begin {
                    increment(&mut self.metrics.duplicate_begins);
                    return Ok(ResyncBeginOutcome::Duplicate);
                }
                increment(&mut self.metrics.conflicting_messages);
                increment(&mut self.metrics.rejected_messages);
                return Err(ResyncTransferError::ConflictingBegin {
                    transfer_id: begin.transfer_id,
                });
            }
            if begin.snapshot_tick < active.begin.snapshot_tick {
                increment(&mut self.metrics.rejected_messages);
                return Err(ResyncTransferError::StaleReplacement {
                    active_tick: active.begin.snapshot_tick,
                    offered_tick: begin.snapshot_tick,
                });
            }
        }

        let previous_transfer_id = self.active.as_ref().map(|active| active.begin.transfer_id);
        self.active = Some(ActiveResyncTransfer::new(begin, now));
        increment(&mut self.metrics.begins_accepted);
        if let Some(previous_transfer_id) = previous_transfer_id {
            increment(&mut self.metrics.superseded_transfers);
            Ok(ResyncBeginOutcome::Superseded {
                previous_transfer_id,
            })
        } else {
            Ok(ResyncBeginOutcome::Started)
        }
    }

    pub fn accept_chunk(
        &mut self,
        chunk: ResyncChunk,
        now: SimTick,
    ) -> ResyncTransferResult<ResyncChunkOutcome> {
        self.observe_clock(now)?;
        self.expire_pre_begin(now);
        if let Err(error) = chunk.validate() {
            increment(&mut self.metrics.rejected_messages);
            return Err(error.into());
        }
        if chunk.match_id != self.match_id {
            increment(&mut self.metrics.rejected_messages);
            return Err(ResyncTransferError::ExpectedMatchMismatch);
        }
        if self
            .active
            .as_ref()
            .is_none_or(|active| active.begin.transfer_id != chunk.transfer_id)
        {
            return self.stage_pre_begin(chunk, now);
        }
        self.accept_active_chunk(chunk, now)
    }

    pub fn accept_input_tail(
        &mut self,
        tail: ResyncInputTail,
        now: SimTick,
    ) -> ResyncTransferResult<ResyncInputTailOutcome> {
        self.observe_clock(now)?;
        self.expire_pre_begin(now);
        if let Err(error) = tail.validate() {
            increment(&mut self.metrics.rejected_messages);
            return Err(error.into());
        }
        if tail.match_id != self.match_id {
            increment(&mut self.metrics.rejected_messages);
            return Err(ResyncTransferError::ExpectedMatchMismatch);
        }
        if self
            .active
            .as_ref()
            .is_none_or(|active| active.begin.transfer_id != tail.transfer_id)
        {
            return self.stage_pre_begin_input_tail(tail, now);
        }
        self.accept_active_input_tail(tail, now)
    }

    /// Applies snapshot chunks and the input tail that arrived on the independent
    /// Resync channel before the matching Control-channel Begin. Call this
    /// immediately after accepting a Begin; it performs no allocation and may
    /// complete the transfer.
    pub fn apply_staged_chunks(
        &mut self,
        now: SimTick,
    ) -> ResyncTransferResult<Option<CompletedResyncTransfer>> {
        self.observe_clock(now)?;
        self.expire_pre_begin(now);
        let Some(begin) = self.active.as_ref().map(|active| active.begin) else {
            return Ok(None);
        };
        loop {
            let next = self
                .pre_begin
                .iter()
                .enumerate()
                .filter_map(|(slot, staged)| {
                    staged
                        .filter(|staged| staged.chunk.transfer_id == begin.transfer_id)
                        .map(|staged| (staged.chunk.chunk_index, slot))
                })
                .min()
                .map(|(_, slot)| slot);
            if let Some(slot) = next {
                let staged = self.remove_pre_begin_slot(slot);
                match self.accept_active_chunk(staged.chunk, now)? {
                    ResyncChunkOutcome::Accepted(_) | ResyncChunkOutcome::Duplicate(_) => {}
                    ResyncChunkOutcome::Complete(completed) => return Ok(Some(completed)),
                    ResyncChunkOutcome::StagedBeforeBegin(_) => {
                        unreachable!("matching staged chunk must enter the active transfer")
                    }
                }
                continue;
            }
            let staged_tail_slot = self.pre_begin_input_tails.iter().position(|staged| {
                staged.is_some_and(|staged| staged.tail.transfer_id == begin.transfer_id)
            });
            let Some(slot) = staged_tail_slot else {
                return Ok(None);
            };
            let staged = self.pre_begin_input_tails[slot]
                .take()
                .expect("selected input-tail staging slot is occupied");
            match self.accept_active_input_tail(staged.tail, now)? {
                ResyncInputTailOutcome::Accepted(_) | ResyncInputTailOutcome::Duplicate(_) => {}
                ResyncInputTailOutcome::Complete(completed) => return Ok(Some(completed)),
                ResyncInputTailOutcome::StagedBeforeBegin(_) => {
                    unreachable!("matching staged input tail must enter the active transfer")
                }
            }
        }
    }

    fn accept_active_chunk(
        &mut self,
        chunk: ResyncChunk,
        now: SimTick,
    ) -> ResyncTransferResult<ResyncChunkOutcome> {
        let active = self
            .active
            .as_mut()
            .expect("active chunk acceptance requires a transfer");
        if let Err(error) = chunk.validate_against(&active.begin) {
            increment(&mut self.metrics.rejected_messages);
            return Err(error.into());
        }
        validate_clock(active.last_progress_tick, now)?;

        let index = usize::from(chunk.chunk_index);
        let offset = index * MAX_RESYNC_CHUNK_BYTES;
        let length = usize::from(chunk.payload_len);
        let end = offset + length;
        debug_assert!(end <= active.bytes.len());
        if active.received[index] {
            let mut duplicate_bytes = [0_u8; MAX_RESYNC_CHUNK_BYTES];
            chunk
                .payload
                .copy_prefix_into(chunk.payload_len, &mut duplicate_bytes)?;
            if active.bytes[offset..end] != duplicate_bytes[..length] {
                increment(&mut self.metrics.conflicting_messages);
                increment(&mut self.metrics.rejected_messages);
                return Err(ResyncTransferError::ConflictingChunk {
                    transfer_id: chunk.transfer_id,
                    chunk_index: chunk.chunk_index,
                });
            }
            increment(&mut self.metrics.duplicate_chunks);
            return Ok(ResyncChunkOutcome::Duplicate(active.progress()));
        }

        chunk
            .payload
            .copy_prefix_into(chunk.payload_len, &mut active.bytes[offset..end])?;
        active.received[index] = true;
        active.received_chunks += 1;
        active.received_bytes += length;
        active.last_progress_tick = now;
        increment(&mut self.metrics.chunks_accepted);
        add(&mut self.metrics.chunk_bytes_accepted, length);

        if active.received_chunks != active.begin.chunk_count || active.input_tail.is_none() {
            return Ok(ResyncChunkOutcome::Accepted(active.progress()));
        }

        let completed = self
            .active
            .take()
            .expect("the completed transfer was active immediately above");
        match decode_completed_transfer(completed, self.peer_id) {
            Ok(completed) => {
                increment(&mut self.metrics.completed_transfers);
                Ok(ResyncChunkOutcome::Complete(completed))
            }
            Err(error) => {
                increment(&mut self.metrics.verification_failures);
                Err(error)
            }
        }
    }

    fn accept_active_input_tail(
        &mut self,
        tail: ResyncInputTail,
        now: SimTick,
    ) -> ResyncTransferResult<ResyncInputTailOutcome> {
        let active = self
            .active
            .as_mut()
            .expect("active input-tail acceptance requires a transfer");
        if let Err(error) = tail.validate_against(&active.begin) {
            increment(&mut self.metrics.rejected_messages);
            return Err(error.into());
        }
        validate_clock(active.last_progress_tick, now)?;
        if let Some(existing) = active.input_tail {
            if existing != tail {
                increment(&mut self.metrics.conflicting_messages);
                increment(&mut self.metrics.rejected_messages);
                return Err(ResyncTransferError::ConflictingInputTail {
                    transfer_id: tail.transfer_id,
                });
            }
            increment(&mut self.metrics.duplicate_input_tails);
            return Ok(ResyncInputTailOutcome::Duplicate(active.progress()));
        }
        active.input_tail = Some(tail);
        active.last_progress_tick = now;
        increment(&mut self.metrics.input_tails_accepted);
        if active.received_chunks != active.begin.chunk_count {
            return Ok(ResyncInputTailOutcome::Accepted(active.progress()));
        }
        let completed = self
            .active
            .take()
            .expect("the completed transfer was active immediately above");
        match decode_completed_transfer(completed, self.peer_id) {
            Ok(completed) => {
                increment(&mut self.metrics.completed_transfers);
                Ok(ResyncInputTailOutcome::Complete(completed))
            }
            Err(error) => {
                increment(&mut self.metrics.verification_failures);
                Err(error)
            }
        }
    }

    fn stage_pre_begin(
        &mut self,
        chunk: ResyncChunk,
        now: SimTick,
    ) -> ResyncTransferResult<ResyncChunkOutcome> {
        debug_assert_eq!(chunk.match_id, self.match_id);
        let first_for_transfer = self
            .pre_begin
            .iter()
            .flatten()
            .copied()
            .find(|staged| staged.chunk.transfer_id == chunk.transfer_id);
        let staged_tail = self
            .pre_begin_input_tails
            .iter()
            .flatten()
            .find(|staged| staged.tail.transfer_id == chunk.transfer_id);
        if let Some(staged_tail) = staged_tail
            && !same_chunk_tail_contract(chunk, staged_tail.tail)
        {
            increment(&mut self.metrics.pre_begin_conflicting_chunks);
            increment(&mut self.metrics.conflicting_messages);
            increment(&mut self.metrics.rejected_messages);
            return Err(ProtocolValidationError::ResyncMismatch.into());
        }
        if let Some(first) = first_for_transfer {
            if !same_chunk_transfer_contract(first.chunk, chunk) {
                increment(&mut self.metrics.pre_begin_conflicting_chunks);
                increment(&mut self.metrics.conflicting_messages);
                increment(&mut self.metrics.rejected_messages);
                return Err(ProtocolValidationError::ResyncMismatch.into());
            }
            if let Some(duplicate) = self.pre_begin.iter().flatten().find(|staged| {
                staged.chunk.transfer_id == chunk.transfer_id
                    && staged.chunk.chunk_index == chunk.chunk_index
            }) {
                if duplicate.chunk == chunk {
                    increment(&mut self.metrics.pre_begin_duplicate_chunks);
                    return Ok(ResyncChunkOutcome::StagedBeforeBegin(
                        self.pre_begin_progress(chunk.transfer_id),
                    ));
                }
                increment(&mut self.metrics.pre_begin_conflicting_chunks);
                increment(&mut self.metrics.conflicting_messages);
                increment(&mut self.metrics.rejected_messages);
                return Err(ResyncTransferError::ConflictingChunk {
                    transfer_id: chunk.transfer_id,
                    chunk_index: chunk.chunk_index,
                });
            }
        } else if staged_tail.is_none()
            && self.pre_begin_transfer_count() >= MAX_PRE_BEGIN_TRANSFERS
        {
            self.evict_oldest_pre_begin_transfer(false);
        }

        let (transfer_chunks, transfer_bytes) = self
            .pre_begin
            .iter()
            .flatten()
            .filter(|staged| staged.chunk.transfer_id == chunk.transfer_id)
            .fold((0_usize, 0_usize), |(count, bytes), staged| {
                (count + 1, bytes + usize::from(staged.chunk.payload_len))
            });
        if transfer_chunks >= usize::from(chunk.chunk_count)
            || transfer_bytes + usize::from(chunk.payload_len) > MAX_RESYNC_SNAPSHOT_BYTES
        {
            increment(&mut self.metrics.rejected_messages);
            return Err(ProtocolValidationError::CapacityExceeded.into());
        }

        // The metadata was validated before this point.  Both limits are fixed
        // protocol constants; neither allocation nor capacity follows an
        // attacker-provided count.
        if self.pre_begin_len == MAX_RESYNC_CHUNKS
            || self.pre_begin_bytes + usize::from(chunk.payload_len) > MAX_RESYNC_SNAPSHOT_BYTES
        {
            self.evict_oldest_pre_begin_transfer(false);
        }
        let Some(slot) = self.pre_begin.iter().position(Option::is_none) else {
            increment(&mut self.metrics.rejected_messages);
            return Err(ProtocolValidationError::CapacityExceeded.into());
        };
        if self.pre_begin_bytes + usize::from(chunk.payload_len) > MAX_RESYNC_SNAPSHOT_BYTES {
            increment(&mut self.metrics.rejected_messages);
            return Err(ProtocolValidationError::CapacityExceeded.into());
        }
        self.pre_begin[slot] = Some(StagedPreBeginChunk {
            chunk,
            staged_tick: now,
        });
        self.pre_begin_len += 1;
        self.pre_begin_bytes += usize::from(chunk.payload_len);
        increment(&mut self.metrics.pre_begin_chunks_staged);
        add(
            &mut self.metrics.pre_begin_bytes_staged,
            usize::from(chunk.payload_len),
        );
        Ok(ResyncChunkOutcome::StagedBeforeBegin(
            self.pre_begin_progress(chunk.transfer_id),
        ))
    }

    fn stage_pre_begin_input_tail(
        &mut self,
        tail: ResyncInputTail,
        now: SimTick,
    ) -> ResyncTransferResult<ResyncInputTailOutcome> {
        debug_assert_eq!(tail.match_id, self.match_id);
        if let Some(chunk) = self
            .pre_begin
            .iter()
            .flatten()
            .find(|staged| staged.chunk.transfer_id == tail.transfer_id)
        {
            if !same_chunk_tail_contract(chunk.chunk, tail) {
                increment(&mut self.metrics.conflicting_messages);
                increment(&mut self.metrics.rejected_messages);
                return Err(ProtocolValidationError::ResyncMismatch.into());
            }
        }
        if let Some(existing) = self
            .pre_begin_input_tails
            .iter()
            .flatten()
            .find(|staged| staged.tail.transfer_id == tail.transfer_id)
        {
            if existing.tail == tail {
                increment(&mut self.metrics.duplicate_input_tails);
                return Ok(ResyncInputTailOutcome::StagedBeforeBegin(
                    self.pre_begin_progress(tail.transfer_id),
                ));
            }
            increment(&mut self.metrics.conflicting_messages);
            increment(&mut self.metrics.rejected_messages);
            return Err(ResyncTransferError::ConflictingInputTail {
                transfer_id: tail.transfer_id,
            });
        }
        if self.pre_begin_transfer_count() >= MAX_PRE_BEGIN_TRANSFERS {
            self.evict_oldest_pre_begin_transfer(false);
        }
        let Some(slot) = self.pre_begin_input_tails.iter().position(Option::is_none) else {
            increment(&mut self.metrics.rejected_messages);
            return Err(ProtocolValidationError::CapacityExceeded.into());
        };
        self.pre_begin_input_tails[slot] = Some(StagedPreBeginInputTail {
            tail,
            staged_tick: now,
        });
        increment(&mut self.metrics.pre_begin_input_tails_staged);
        Ok(ResyncInputTailOutcome::StagedBeforeBegin(
            self.pre_begin_progress(tail.transfer_id),
        ))
    }

    fn pre_begin_progress(&self, transfer_id: TransferId) -> PreBeginResyncProgress {
        let mut staged_chunks = 0_u16;
        let mut staged_bytes = 0_u32;
        let mut oldest_staged_tick = SimTick(u64::MAX);
        for staged in self.pre_begin.iter().flatten() {
            if staged.chunk.transfer_id != transfer_id {
                continue;
            }
            staged_chunks = staged_chunks.saturating_add(1);
            staged_bytes = staged_bytes.saturating_add(u32::from(staged.chunk.payload_len));
            oldest_staged_tick = oldest_staged_tick.min(staged.staged_tick);
        }
        for staged in self.pre_begin_input_tails.iter().flatten() {
            if staged.tail.transfer_id == transfer_id {
                oldest_staged_tick = oldest_staged_tick.min(staged.staged_tick);
            }
        }
        PreBeginResyncProgress {
            transfer_id,
            staged_chunks,
            staged_bytes,
            staged_input_tail: self
                .pre_begin_input_tails
                .iter()
                .flatten()
                .any(|staged| staged.tail.transfer_id == transfer_id),
            oldest_staged_tick,
        }
    }

    fn pre_begin_transfer_count(&self) -> usize {
        let mut ids = [None; MAX_PRE_BEGIN_TRANSFERS];
        let mut count = 0;
        for staged in self.pre_begin.iter().flatten() {
            if ids[..count].contains(&Some(staged.chunk.transfer_id)) {
                continue;
            }
            if count == ids.len() {
                return count;
            }
            ids[count] = Some(staged.chunk.transfer_id);
            count += 1;
        }
        for staged in self.pre_begin_input_tails.iter().flatten() {
            if ids[..count].contains(&Some(staged.tail.transfer_id)) {
                continue;
            }
            if count == ids.len() {
                return count;
            }
            ids[count] = Some(staged.tail.transfer_id);
            count += 1;
        }
        count
    }

    fn observe_clock(&mut self, now: SimTick) -> ResyncTransferResult<()> {
        validate_clock(self.last_observed_tick, now)?;
        self.last_observed_tick = now;
        Ok(())
    }

    fn expire_pre_begin(&mut self, now: SimTick) {
        let timeout = self.timeout_ticks.min(PRE_BEGIN_CHUNK_TIMEOUT_TICKS);
        loop {
            let expired_chunk = self
                .pre_begin
                .iter()
                .flatten()
                .filter(|staged| now.get().saturating_sub(staged.staged_tick.get()) >= timeout)
                .min_by_key(|staged| (staged.staged_tick, staged.chunk.transfer_id.get()))
                .map(|staged| (staged.staged_tick, staged.chunk.transfer_id));
            let expired_tail = self
                .pre_begin_input_tails
                .iter()
                .flatten()
                .filter(|staged| now.get().saturating_sub(staged.staged_tick.get()) >= timeout)
                .map(|staged| (staged.staged_tick, staged.tail.transfer_id))
                .min_by_key(|(tick, transfer_id)| (*tick, transfer_id.get()));
            let expired = [expired_chunk, expired_tail]
                .into_iter()
                .flatten()
                .min_by_key(|(tick, transfer_id)| (*tick, transfer_id.get()))
                .map(|(_, transfer_id)| transfer_id);
            let Some(transfer_id) = expired else {
                break;
            };
            self.remove_pre_begin_transfer(transfer_id, true);
        }
    }

    fn evict_oldest_pre_begin_transfer(&mut self, expired: bool) {
        let chunk = self
            .pre_begin
            .iter()
            .flatten()
            .map(|staged| (staged.staged_tick, staged.chunk.transfer_id))
            .min_by_key(|(tick, transfer_id)| (*tick, transfer_id.get()));
        let tail = self
            .pre_begin_input_tails
            .iter()
            .flatten()
            .map(|staged| (staged.staged_tick, staged.tail.transfer_id))
            .min_by_key(|(tick, transfer_id)| (*tick, transfer_id.get()));
        let transfer_id = [chunk, tail]
            .into_iter()
            .flatten()
            .min_by_key(|(tick, transfer_id)| (*tick, transfer_id.get()))
            .map(|(_, transfer_id)| transfer_id);
        if let Some(transfer_id) = transfer_id {
            self.remove_pre_begin_transfer(transfer_id, expired);
        }
    }

    fn remove_pre_begin_transfer(&mut self, transfer_id: TransferId, expired: bool) {
        let mut chunks = 0_usize;
        let mut bytes = 0_usize;
        let mut input_tails = 0_usize;
        for slot in 0..self.pre_begin.len() {
            if self.pre_begin[slot].is_some_and(|staged| staged.chunk.transfer_id == transfer_id) {
                let removed = self.remove_pre_begin_slot(slot);
                chunks += 1;
                bytes += usize::from(removed.chunk.payload_len);
            }
        }
        for slot in &mut self.pre_begin_input_tails {
            if slot.is_some_and(|staged| staged.tail.transfer_id == transfer_id) {
                *slot = None;
                input_tails += 1;
            }
        }
        if chunks == 0 && input_tails == 0 {
            return;
        }
        if expired {
            increment(&mut self.metrics.pre_begin_transfers_expired);
            add(&mut self.metrics.pre_begin_chunks_expired, chunks);
            add(&mut self.metrics.pre_begin_bytes_expired, bytes);
            add(&mut self.metrics.pre_begin_input_tails_expired, input_tails);
        } else {
            increment(&mut self.metrics.pre_begin_transfers_evicted);
            add(&mut self.metrics.pre_begin_chunks_evicted, chunks);
            add(&mut self.metrics.pre_begin_bytes_evicted, bytes);
            add(&mut self.metrics.pre_begin_input_tails_evicted, input_tails);
        }
    }

    fn remove_pre_begin_slot(&mut self, slot: usize) -> StagedPreBeginChunk {
        let removed = self.pre_begin[slot]
            .take()
            .expect("pre-begin removal targets an occupied slot");
        self.pre_begin_len -= 1;
        self.pre_begin_bytes -= usize::from(removed.chunk.payload_len);
        removed
    }

    /// Expires a transfer after `timeout_ticks` without new unique chunk data.
    /// Duplicate messages deliberately do not extend the deadline.
    pub fn expire_if_timed_out(
        &mut self,
        now: SimTick,
    ) -> ResyncTransferResult<Option<ResetResyncTransfer>> {
        self.observe_clock(now)?;
        self.expire_pre_begin(now);
        let Some(active) = self.active.as_ref() else {
            return Ok(None);
        };
        validate_clock(active.last_progress_tick, now)?;
        if now.get() - active.last_progress_tick.get() < self.timeout_ticks {
            return Ok(None);
        }
        let reset = active.reset_metadata();
        self.active = None;
        increment(&mut self.metrics.timed_out_transfers);
        Ok(Some(reset))
    }

    pub fn reset(&mut self) -> Option<ResetResyncTransfer> {
        let reset = self
            .active
            .as_ref()
            .map(ActiveResyncTransfer::reset_metadata);
        if reset.is_some()
            || self.pre_begin_len != 0
            || self.pre_begin_input_tails.iter().any(Option::is_some)
        {
            self.active = None;
            self.clear_pre_begin();
            increment(&mut self.metrics.manual_resets);
        }
        reset
    }

    fn clear_pre_begin(&mut self) {
        for slot in self.pre_begin.iter_mut() {
            *slot = None;
        }
        self.pre_begin_input_tails.fill(None);
        self.pre_begin_len = 0;
        self.pre_begin_bytes = 0;
    }
}

fn same_chunk_transfer_contract(left: ResyncChunk, right: ResyncChunk) -> bool {
    left.match_id == right.match_id
        && left.transfer_id == right.transfer_id
        && left.snapshot_tick == right.snapshot_tick
        && left.snapshot_hash == right.snapshot_hash
        && left.chunk_count == right.chunk_count
}

fn same_chunk_tail_contract(chunk: ResyncChunk, tail: ResyncInputTail) -> bool {
    chunk.match_id == tail.match_id
        && chunk.transfer_id == tail.transfer_id
        && chunk.snapshot_tick == tail.snapshot_tick
        && chunk.snapshot_hash == tail.snapshot_hash
}

fn validate_clock(previous: SimTick, now: SimTick) -> ResyncTransferResult<()> {
    if now < previous {
        Err(ResyncTransferError::ClockRegressed { previous, now })
    } else {
        Ok(())
    }
}

fn decode_completed_transfer(
    transfer: ActiveResyncTransfer,
    peer_id: PeerId,
) -> ResyncTransferResult<CompletedResyncTransfer> {
    if transfer.received_bytes != transfer.bytes.len()
        || transfer.received_chunks != transfer.begin.chunk_count
    {
        return Err(ResyncTransferError::IncompleteTransfer);
    }
    let input_tail = transfer
        .input_tail
        .ok_or(ResyncTransferError::MissingInputTail)?;
    input_tail.validate_against(&transfer.begin)?;
    let snapshot = CanonicalSnapshot::decode(&transfer.bytes)?;
    if snapshot.header.match_id != *transfer.begin.match_id.as_bytes() {
        return Err(ResyncTransferError::SnapshotMatchMismatch);
    }
    if snapshot.header.tick != transfer.begin.snapshot_tick {
        return Err(ResyncTransferError::SnapshotTickMismatch {
            expected: transfer.begin.snapshot_tick,
            actual: snapshot.header.tick,
        });
    }
    let actual_hash = StateHash(hash_canonical_bytes(&transfer.bytes));
    if actual_hash != transfer.begin.snapshot_hash {
        return Err(ResyncTransferError::SnapshotHashMismatch {
            expected: transfer.begin.snapshot_hash,
            actual: actual_hash,
        });
    }
    let applied = ResyncApplied {
        match_id: transfer.begin.match_id,
        transfer_id: transfer.begin.transfer_id,
        peer_id,
        snapshot_tick: transfer.begin.snapshot_tick,
        snapshot_hash: transfer.begin.snapshot_hash,
    };
    applied.validate()?;
    Ok(CompletedResyncTransfer {
        snapshot,
        input_tail,
        applied,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::determinism::{FighterId, SimEntityId, SimEntityKind};
    use crate::network_protocol::{
        CommittedInputRecord, CommittedInputSource, InputButtons, InputFrame, InputSequence,
        MAX_SEATS, QuantizedAxis, SeatId,
    };
    use crate::snapshot::{
        ArenaRuntimeSnapshot, DynamicObjectSnapshot, FighterSnapshot, MatchStateSnapshot,
        MatchStatsSnapshot, PoolAllocatorSnapshot, SIM_ENTITY_KIND_COUNT, SnapshotHeader,
    };

    const MATCH_BYTES: [u8; 16] = *b"resync-test-0001";
    const OTHER_MATCH_BYTES: [u8; 16] = *b"resync-test-0002";

    fn match_id() -> MatchId {
        MatchId::new(MATCH_BYTES).unwrap()
    }

    fn peer_id() -> PeerId {
        PeerId::new(7).unwrap()
    }

    fn transfer_id(value: u32) -> TransferId {
        TransferId::new(value).unwrap()
    }

    fn fixture_with_match(
        tick: u64,
        match_bytes: [u8; 16],
        hitbox_capacity: u32,
    ) -> CanonicalSnapshot {
        let allocators = SimEntityKind::ALL
            .into_iter()
            .map(|kind| {
                let capacity = if kind == SimEntityKind::Hitbox {
                    hitbox_capacity
                } else {
                    1
                };
                PoolAllocatorSnapshot::empty(kind, capacity).unwrap()
            })
            .collect();
        CanonicalSnapshot {
            header: SnapshotHeader::new(1, 1, 99, match_bytes, SimTick(tick), 123),
            match_state: MatchStateSnapshot::default(),
            fighters: FighterId::ALL.map(FighterSnapshot::empty),
            arena: ArenaRuntimeSnapshot::default(),
            allocators,
            dynamic_objects: Vec::new(),
            rng_streams: Vec::new(),
            stats: MatchStatsSnapshot::default(),
        }
    }

    fn fixture(tick: u64) -> CanonicalSnapshot {
        fixture_with_match(tick, MATCH_BYTES, 128)
    }

    fn oversized_fixture(tick: u64) -> CanonicalSnapshot {
        const OBJECTS: u32 = 1_024;
        let mut snapshot = fixture_with_match(tick, MATCH_BYTES, OBJECTS);
        let allocator = &mut snapshot.allocators[SimEntityKind::Hitbox.code() as usize];
        let mut objects = Vec::with_capacity(OBJECTS as usize);
        for index in 0..OBJECTS {
            let id = SimEntityId::new(SimEntityKind::Hitbox, index, 1);
            allocator.set_slot(index, 1, true).unwrap();
            objects.push(DynamicObjectSnapshot::empty(id));
        }
        snapshot.dynamic_objects = objects;
        snapshot
    }

    fn request(snapshot_tick: u64) -> ResyncRequest {
        ResyncRequest {
            match_id: match_id(),
            peer_id: peer_id(),
            reason: ResyncReason::HashMismatch,
            last_confirmed_tick: SimTick(snapshot_tick.saturating_sub(2)),
            last_confirmed_hash: StateHash(55),
        }
    }

    fn input_windows(tick: SimTick) -> [CommittedSeatInputWindow; MAX_SEATS] {
        std::array::from_fn(|seat| {
            let start = tick
                .0
                .saturating_sub((MAX_RESYNC_INPUT_TAIL_TICKS - 1) as u64);
            let len = (tick.0 - start + 1) as usize;
            let mut records = [CommittedInputRecord::default(); MAX_RESYNC_INPUT_TAIL_TICKS];
            for (offset, record) in records[..len].iter_mut().enumerate() {
                *record = CommittedInputRecord {
                    frame: InputFrame {
                        tick: SimTick(tick.0 - offset as u64),
                        seat: SeatId::new(seat as u8).unwrap(),
                        movement_x: QuantizedAxis::new(seat as i8).unwrap(),
                        movement_y: QuantizedAxis::default(),
                        held_buttons: InputButtons::new(InputButtons::GUARD).unwrap(),
                        pressed_buttons: InputButtons::default(),
                        released_buttons: InputButtons::default(),
                        sequence: InputSequence((tick.0 - offset as u64) as u16),
                    },
                    fighter: FighterId::new(seat as u8).unwrap(),
                    source: CommittedInputSource::MissingSubstitute,
                };
            }
            CommittedSeatInputWindow::from_newest_first(&records[..len]).unwrap()
        })
    }

    fn transfer(snapshot: &CanonicalSnapshot) -> AuthorityResyncTransfer {
        let windows = input_windows(snapshot.header.tick);
        AuthorityResyncTransfer::from_snapshot(
            request(snapshot.header.tick.get()),
            transfer_id(11),
            snapshot,
            &windows,
        )
        .unwrap()
    }

    fn chunks_for(begin: ResyncBegin, bytes: &[u8]) -> Vec<ResyncChunk> {
        (0..begin.chunk_count)
            .map(|chunk_index| {
                let offset = usize::from(chunk_index) * MAX_RESYNC_CHUNK_BYTES;
                let end = (offset + MAX_RESYNC_CHUNK_BYTES).min(bytes.len());
                let (payload, payload_len) =
                    ResyncChunkPayload::from_bytes(&bytes[offset..end]).unwrap();
                ResyncChunk {
                    match_id: begin.match_id,
                    transfer_id: begin.transfer_id,
                    snapshot_tick: begin.snapshot_tick,
                    snapshot_hash: begin.snapshot_hash,
                    chunk_index,
                    chunk_count: begin.chunk_count,
                    payload_len,
                    payload,
                }
            })
            .collect()
    }

    fn feed_all(
        assembler: &mut ClientResyncAssembler,
        chunks: impl IntoIterator<Item = ResyncChunk>,
        input_tail: ResyncInputTail,
        start_tick: u64,
    ) -> ResyncTransferResult<CompletedResyncTransfer> {
        let mut completed = None;
        let mut next_tick = start_tick;
        for chunk in chunks {
            match assembler.accept_chunk(chunk, SimTick(next_tick))? {
                ResyncChunkOutcome::Complete(value) => completed = Some(value),
                ResyncChunkOutcome::Accepted(_)
                | ResyncChunkOutcome::Duplicate(_)
                | ResyncChunkOutcome::StagedBeforeBegin(_) => {}
            }
            next_tick = next_tick.saturating_add(1);
        }
        if completed.is_none() {
            if let ResyncInputTailOutcome::Complete(value) =
                assembler.accept_input_tail(input_tail, SimTick(next_tick))?
            {
                completed = Some(value);
            }
        }
        completed.ok_or(ResyncTransferError::IncompleteTransfer)
    }

    #[test]
    fn authority_iterator_and_out_of_order_client_round_trip() {
        let snapshot = fixture(240);
        let mut authority = transfer(&snapshot);
        let begin = authority.begin();
        let input_tail = authority.input_tail();
        let chunks: Vec<_> = authority.chunks().collect();
        assert!(chunks.len() > 1);
        assert_eq!(chunks.len(), usize::from(begin.chunk_count));
        assert_eq!(
            chunks
                .iter()
                .map(|chunk| usize::from(chunk.payload_len))
                .sum::<usize>(),
            authority.snapshot_bytes()
        );

        let mut assembler = ClientResyncAssembler::new(match_id(), peer_id(), 30).unwrap();
        assert_eq!(
            assembler.accept_begin(begin, SimTick(1_000)).unwrap(),
            ResyncBeginOutcome::Started
        );
        let duplicate = *chunks.last().unwrap();
        assert!(matches!(
            assembler.accept_chunk(duplicate, SimTick(1_001)).unwrap(),
            ResyncChunkOutcome::Accepted(_)
        ));
        assert!(matches!(
            assembler.accept_chunk(duplicate, SimTick(1_002)).unwrap(),
            ResyncChunkOutcome::Duplicate(_)
        ));

        let completed = feed_all(
            &mut assembler,
            chunks[..chunks.len() - 1].iter().rev().copied(),
            input_tail,
            1_003,
        )
        .unwrap();
        assert_eq!(completed.snapshot, snapshot);
        authority.validate_applied(&completed.applied).unwrap();
        assert_eq!(
            authority.metrics().chunks_emitted,
            u64::from(begin.chunk_count)
        );
        assert_eq!(authority.metrics().applied_acknowledgements, 1);
        assert_eq!(assembler.metrics().duplicate_chunks, 1);
        assert_eq!(assembler.metrics().completed_transfers, 1);
        assert_eq!(assembler.active_progress(), None);
    }

    #[test]
    fn chunks_may_arrive_before_cross_channel_begin_without_loss() {
        let snapshot = fixture(241);
        let mut authority = transfer(&snapshot);
        let begin = authority.begin();
        let input_tail = authority.input_tail();
        let chunks: Vec<_> = authority.chunks().collect();
        let mut assembler = ClientResyncAssembler::new(match_id(), peer_id(), 120).unwrap();

        for (offset, chunk) in chunks.iter().copied().enumerate() {
            assert!(matches!(
                assembler
                    .accept_chunk(chunk, SimTick(10 + offset as u64))
                    .unwrap(),
                ResyncChunkOutcome::StagedBeforeBegin(_)
            ));
        }
        assert!(matches!(
            assembler
                .accept_input_tail(input_tail, SimTick(10 + chunks.len() as u64))
                .unwrap(),
            ResyncInputTailOutcome::StagedBeforeBegin(_)
        ));
        assert_eq!(assembler.staged_pre_begin_chunks(), chunks.len());
        assert_eq!(
            assembler.staged_pre_begin_bytes(),
            snapshot.encode().unwrap().len()
        );

        let begin_tick = SimTick(11 + chunks.len() as u64);
        assert_eq!(
            assembler.accept_begin(begin, begin_tick).unwrap(),
            ResyncBeginOutcome::Started
        );
        let completed = assembler
            .apply_staged_chunks(begin_tick)
            .unwrap()
            .expect("all pre-begin chunks complete the transfer");
        assert_eq!(completed.snapshot, snapshot);
        assert_eq!(assembler.staged_pre_begin_chunks(), 0);
        assert_eq!(assembler.staged_pre_begin_bytes(), 0);
        assert_eq!(
            assembler.metrics().pre_begin_chunks_staged,
            chunks.len() as u64
        );
        assert_eq!(assembler.metrics().pre_begin_input_tails_staged, 1);
    }

    #[test]
    fn complete_snapshot_waits_for_identity_bound_input_tail() {
        let snapshot = fixture(260);
        let mut authority = transfer(&snapshot);
        let begin = authority.begin();
        let input_tail = authority.input_tail();
        let chunks: Vec<_> = authority.chunks().collect();
        let mut assembler = ClientResyncAssembler::new(match_id(), peer_id(), 30).unwrap();
        assembler.accept_begin(begin, SimTick(1)).unwrap();
        for (offset, chunk) in chunks.into_iter().enumerate() {
            assert!(matches!(
                assembler
                    .accept_chunk(chunk, SimTick(2 + offset as u64))
                    .unwrap(),
                ResyncChunkOutcome::Accepted(_)
            ));
        }
        let progress = assembler
            .active_progress()
            .expect("snapshot bytes alone cannot complete a resync");
        assert_eq!(progress.received_chunks, begin.chunk_count);
        assert!(!progress.received_input_tail);

        let completed = match assembler
            .accept_input_tail(input_tail, SimTick(200))
            .unwrap()
        {
            ResyncInputTailOutcome::Complete(completed) => completed,
            other => panic!("input tail should complete byte-ready transfer: {other:?}"),
        };
        assert_eq!(completed.snapshot, snapshot);
        assert_eq!(completed.input_tail, input_tail);

        let mut wrong_identity = input_tail;
        wrong_identity.snapshot_hash = StateHash(input_tail.snapshot_hash.0 ^ 1);
        let mut second = ClientResyncAssembler::new(match_id(), peer_id(), 30).unwrap();
        second.accept_begin(begin, SimTick(1)).unwrap();
        assert!(matches!(
            second.accept_input_tail(wrong_identity, SimTick(2)),
            Err(ResyncTransferError::Protocol(
                ProtocolValidationError::ResyncMismatch
            ))
        ));
    }

    #[test]
    fn hostile_unknown_transfer_flood_is_bounded_conflict_checked_and_expires() {
        let snapshot = fixture(242);
        let mut authority = transfer(&snapshot);
        let template = authority.chunks().next().unwrap();
        let mut assembler = ClientResyncAssembler::new(match_id(), peer_id(), 300).unwrap();

        for transfer in 1..=64 {
            let mut chunk = template;
            chunk.transfer_id = transfer_id(transfer);
            assert!(matches!(
                assembler
                    .accept_chunk(chunk, SimTick(u64::from(transfer)))
                    .unwrap(),
                ResyncChunkOutcome::StagedBeforeBegin(_)
            ));
            assert!(assembler.staged_pre_begin_chunks() <= MAX_RESYNC_CHUNKS);
            assert!(assembler.staged_pre_begin_bytes() <= MAX_RESYNC_SNAPSHOT_BYTES);
            assert!(assembler.pre_begin_transfer_count() <= MAX_PRE_BEGIN_TRANSFERS);
        }
        assert!(assembler.metrics().pre_begin_transfers_evicted > 0);

        let retained = assembler
            .pre_begin
            .iter()
            .flatten()
            .next()
            .expect("the bounded flood retains its newest transfer")
            .chunk;
        let before = assembler.staged_pre_begin_chunks();
        assert!(matches!(
            assembler.accept_chunk(retained, SimTick(65)).unwrap(),
            ResyncChunkOutcome::StagedBeforeBegin(_)
        ));
        assert_eq!(assembler.staged_pre_begin_chunks(), before);
        assert_eq!(assembler.metrics().pre_begin_duplicate_chunks, 1);

        let mut conflict = retained;
        let conflicting_bytes = vec![0xA5; usize::from(conflict.payload_len)];
        (conflict.payload, conflict.payload_len) =
            ResyncChunkPayload::from_bytes(&conflicting_bytes).unwrap();
        assert!(matches!(
            assembler.accept_chunk(conflict, SimTick(66)),
            Err(ResyncTransferError::ConflictingChunk { .. })
        ));
        assert_eq!(assembler.staged_pre_begin_chunks(), before);

        assembler
            .expire_if_timed_out(SimTick(66 + PRE_BEGIN_CHUNK_TIMEOUT_TICKS))
            .unwrap();
        assert_eq!(assembler.staged_pre_begin_chunks(), 0);
        assert!(assembler.metrics().pre_begin_transfers_expired > 0);
    }

    #[test]
    fn duplicate_chunk_conflict_is_rejected_without_overwriting_good_data() {
        let snapshot = fixture(300);
        let mut authority = transfer(&snapshot);
        let begin = authority.begin();
        let chunks: Vec<_> = authority.chunks().collect();
        let original = chunks[0];
        let mut altered_bytes = vec![0; usize::from(original.payload_len)];
        original
            .payload
            .copy_prefix_into(original.payload_len, &mut altered_bytes)
            .unwrap();
        altered_bytes[0] ^= 0xff;
        let (payload, payload_len) = ResyncChunkPayload::from_bytes(&altered_bytes).unwrap();
        let conflicting = ResyncChunk {
            payload,
            payload_len,
            ..original
        };

        let mut assembler = ClientResyncAssembler::new(match_id(), peer_id(), 30).unwrap();
        assembler.accept_begin(begin, SimTick(10)).unwrap();
        assembler.accept_chunk(original, SimTick(11)).unwrap();
        assert_eq!(
            assembler.accept_chunk(conflicting, SimTick(12)),
            Err(ResyncTransferError::ConflictingChunk {
                transfer_id: begin.transfer_id,
                chunk_index: 0,
            })
        );
        let completed = feed_all(
            &mut assembler,
            chunks.into_iter().skip(1),
            authority.input_tail(),
            13,
        )
        .unwrap();
        assert_eq!(completed.snapshot, snapshot);
        assert_eq!(assembler.metrics().conflicting_messages, 1);
    }

    #[test]
    fn begin_conflicts_and_stale_replacements_preserve_active_transfer() {
        let snapshot = fixture(400);
        let authority = transfer(&snapshot);
        let begin = authority.begin();
        let mut assembler = ClientResyncAssembler::new(match_id(), peer_id(), 30).unwrap();
        assembler.accept_begin(begin, SimTick(20)).unwrap();
        assert_eq!(
            assembler.accept_begin(
                ResyncBegin {
                    snapshot_hash: StateHash(begin.snapshot_hash.0 ^ 1),
                    ..begin
                },
                SimTick(21),
            ),
            Err(ResyncTransferError::ConflictingBegin {
                transfer_id: begin.transfer_id,
            })
        );
        assert_eq!(assembler.active_progress().unwrap().begin, begin);

        let stale = ResyncBegin {
            transfer_id: transfer_id(12),
            snapshot_tick: SimTick(begin.snapshot_tick.get() - 1),
            recent_input_start: SimTick(begin.recent_input_start.get() - 1),
            recent_input_end: SimTick(begin.recent_input_end.get() - 1),
            ..begin
        };
        assert_eq!(
            assembler.accept_begin(stale, SimTick(22)),
            Err(ResyncTransferError::StaleReplacement {
                active_tick: begin.snapshot_tick,
                offered_tick: stale.snapshot_tick,
            })
        );
        assert_eq!(assembler.active_progress().unwrap().begin, begin);
    }

    #[test]
    fn completed_snapshot_tick_and_hash_are_verified() {
        let snapshot = fixture(500);
        let authority = transfer(&snapshot);
        let original_begin = authority.begin();
        let encoded = snapshot.encode().unwrap();

        let bad_tick_begin = ResyncBegin {
            snapshot_tick: SimTick(original_begin.snapshot_tick.get() + 1),
            recent_input_start: SimTick(original_begin.recent_input_start.get() + 1),
            recent_input_end: SimTick(original_begin.snapshot_tick.get() + 1),
            ..original_begin
        };
        let mut assembler = ClientResyncAssembler::new(match_id(), peer_id(), 30).unwrap();
        assembler.accept_begin(bad_tick_begin, SimTick(30)).unwrap();
        assert_eq!(
            feed_all(
                &mut assembler,
                chunks_for(bad_tick_begin, &encoded),
                ResyncInputTail::new(
                    &bad_tick_begin,
                    &input_windows(bad_tick_begin.snapshot_tick)
                )
                .unwrap(),
                31,
            ),
            Err(ResyncTransferError::SnapshotTickMismatch {
                expected: bad_tick_begin.snapshot_tick,
                actual: snapshot.header.tick,
            })
        );
        assert_eq!(assembler.active_progress(), None);

        let bad_hash_begin = ResyncBegin {
            snapshot_hash: StateHash(original_begin.snapshot_hash.0 ^ 1),
            ..original_begin
        };
        assembler.accept_begin(bad_hash_begin, SimTick(50)).unwrap();
        assert!(matches!(
            feed_all(
                &mut assembler,
                chunks_for(bad_hash_begin, &encoded),
                ResyncInputTail::new(
                    &bad_hash_begin,
                    &input_windows(bad_hash_begin.snapshot_tick)
                )
                .unwrap(),
                51,
            ),
            Err(ResyncTransferError::SnapshotHashMismatch { .. })
        ));
        assert_eq!(assembler.metrics().verification_failures, 2);
    }

    #[test]
    fn snapshot_match_is_verified_after_canonical_decode() {
        let snapshot = fixture_with_match(550, OTHER_MATCH_BYTES, 128);
        let encoded = snapshot.encode().unwrap();
        let chunk_count = encoded.len().div_ceil(MAX_RESYNC_CHUNK_BYTES) as u16;
        let begin = ResyncBegin {
            match_id: match_id(),
            transfer_id: transfer_id(20),
            snapshot_tick: snapshot.header.tick,
            snapshot_hash: StateHash(hash_canonical_bytes(&encoded)),
            snapshot_bytes: encoded.len() as u32,
            chunk_count,
            recent_input_start: SimTick(546),
            recent_input_end: snapshot.header.tick,
        };
        let mut assembler = ClientResyncAssembler::new(match_id(), peer_id(), 30).unwrap();
        assembler.accept_begin(begin, SimTick(70)).unwrap();
        assert_eq!(
            feed_all(
                &mut assembler,
                chunks_for(begin, &encoded),
                ResyncInputTail::new(&begin, &input_windows(begin.snapshot_tick)).unwrap(),
                71,
            ),
            Err(ResyncTransferError::SnapshotMatchMismatch)
        );
    }

    #[test]
    fn timeout_duplicate_and_manual_reset_are_bounded_and_observable() {
        let snapshot = fixture(600);
        let mut authority = transfer(&snapshot);
        let begin = authority.begin();
        let first = authority.chunks().next().unwrap();
        let mut assembler = ClientResyncAssembler::new(match_id(), peer_id(), 5).unwrap();
        assembler.accept_begin(begin, SimTick(100)).unwrap();
        assembler.accept_chunk(first, SimTick(101)).unwrap();
        assembler.accept_chunk(first, SimTick(104)).unwrap();
        assert_eq!(assembler.expire_if_timed_out(SimTick(105)).unwrap(), None);
        let timed_out = assembler
            .expire_if_timed_out(SimTick(106))
            .unwrap()
            .unwrap();
        assert_eq!(timed_out.begin, begin);
        assert_eq!(timed_out.received_chunks, 1);
        assert_eq!(assembler.metrics().timed_out_transfers, 1);

        assembler.accept_begin(begin, SimTick(110)).unwrap();
        assert_eq!(assembler.reset().unwrap().begin, begin);
        assert_eq!(assembler.metrics().manual_resets, 1);
        assert_eq!(assembler.reset(), None);
    }

    #[test]
    fn hostile_metadata_is_rejected_before_allocation_or_mutation() {
        let snapshot = fixture(700);
        let mut authority = transfer(&snapshot);
        let begin = authority.begin();
        let chunk = authority.chunks().next().unwrap();
        let mut assembler = ClientResyncAssembler::new(match_id(), peer_id(), 30).unwrap();
        assert!(matches!(
            assembler.accept_chunk(chunk, SimTick(1)),
            Ok(ResyncChunkOutcome::StagedBeforeBegin(_))
        ));
        assert_eq!(assembler.active_progress(), None);
        assert_eq!(assembler.staged_pre_begin_chunks(), 1);

        let too_large = ResyncBegin {
            snapshot_bytes: MAX_RESYNC_SNAPSHOT_BYTES as u32 + 1,
            ..begin
        };
        assert_eq!(
            assembler.accept_begin(too_large, SimTick(2)),
            Err(ResyncTransferError::Protocol(
                ProtocolValidationError::SnapshotTooLarge
            ))
        );
        assert_eq!(assembler.active_progress(), None);

        assembler.accept_begin(begin, SimTick(3)).unwrap();
        let wrong_transfer = ResyncChunk {
            transfer_id: transfer_id(99),
            ..chunk
        };
        assert!(matches!(
            assembler.accept_chunk(wrong_transfer, SimTick(4)),
            Ok(ResyncChunkOutcome::StagedBeforeBegin(_))
        ));
        assert_eq!(assembler.active_progress().unwrap().received_chunks, 0);
        assert_eq!(assembler.staged_pre_begin_chunks(), 2);
    }

    #[test]
    fn clock_regression_does_not_change_transfer_state() {
        let snapshot = fixture(750);
        let authority = transfer(&snapshot);
        let begin = authority.begin();
        let mut assembler = ClientResyncAssembler::new(match_id(), peer_id(), 30).unwrap();
        assembler.accept_begin(begin, SimTick(100)).unwrap();
        assert_eq!(
            assembler.expire_if_timed_out(SimTick(99)),
            Err(ResyncTransferError::ClockRegressed {
                previous: SimTick(100),
                now: SimTick(99),
            })
        );
        assert_eq!(assembler.active_progress().unwrap().begin, begin);
    }

    #[test]
    fn authority_rejects_request_mismatch_ahead_tick_and_oversized_snapshot() {
        let snapshot = fixture(800);
        let windows = input_windows(snapshot.header.tick);
        let mut wrong_match_request = request(800);
        wrong_match_request.match_id = MatchId::new(OTHER_MATCH_BYTES).unwrap();
        assert!(matches!(
            AuthorityResyncTransfer::from_snapshot(
                wrong_match_request,
                transfer_id(1),
                &snapshot,
                &windows,
            ),
            Err(ResyncTransferError::RequestMatchMismatch)
        ));

        let mut ahead_request = request(800);
        ahead_request.last_confirmed_tick = SimTick(801);
        assert!(matches!(
            AuthorityResyncTransfer::from_snapshot(
                ahead_request,
                transfer_id(1),
                &snapshot,
                &windows,
            ),
            Err(ResyncTransferError::RequestAheadOfSnapshot { .. })
        ));

        let oversized = oversized_fixture(800);
        let oversized_windows = input_windows(oversized.header.tick);
        assert!(oversized.encode().unwrap().len() > MAX_RESYNC_SNAPSHOT_BYTES);
        assert!(matches!(
            AuthorityResyncTransfer::from_snapshot(
                request(800),
                transfer_id(1),
                &oversized,
                &oversized_windows,
            ),
            Err(ResyncTransferError::SnapshotTooLarge { .. })
        ));
    }

    #[test]
    fn applied_acknowledgement_must_match_transfer_and_request_peer() {
        let snapshot = fixture(900);
        let mut authority = transfer(&snapshot);
        let begin = authority.begin();
        let valid = ResyncApplied {
            match_id: begin.match_id,
            transfer_id: begin.transfer_id,
            peer_id: peer_id(),
            snapshot_tick: begin.snapshot_tick,
            snapshot_hash: begin.snapshot_hash,
        };
        let invalid = ResyncApplied {
            peer_id: PeerId::new(8).unwrap(),
            ..valid
        };
        assert_eq!(
            authority.validate_applied(&invalid),
            Err(ResyncTransferError::AppliedMismatch)
        );
        authority.validate_applied(&valid).unwrap();
        assert_eq!(authority.metrics().applied_acknowledgements, 1);
        assert_eq!(authority.metrics().rejected_messages, 1);
    }

    #[test]
    fn request_builder_and_static_bounds_remain_valid() {
        let assembler = ClientResyncAssembler::with_default_timeout(match_id(), peer_id()).unwrap();
        let request = assembler.make_request(ResyncReason::Reconnect, SimTick(12), StateHash(34));
        request.validate().unwrap();
        assert_eq!(request.match_id, match_id());
        assert_eq!(request.peer_id, peer_id());
        assert_eq!(assembler.timeout_ticks(), DEFAULT_RESYNC_TIMEOUT_TICKS);
        assert_eq!(SIM_ENTITY_KIND_COUNT, SimEntityKind::ALL.len());
        assert_eq!(
            MAX_RESYNC_CHUNKS * MAX_RESYNC_CHUNK_BYTES,
            MAX_RESYNC_SNAPSHOT_BYTES
        );
        assert_eq!(
            ClientResyncAssembler::new(match_id(), peer_id(), 0).err(),
            Some(ResyncTransferError::InvalidTimeout)
        );
    }
}
