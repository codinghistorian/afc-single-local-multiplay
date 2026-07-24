//! Bounded authoritative state-delta production and client reconstruction.
//!
//! The state channel is unreliable and latest-wins. An authority therefore
//! retains a bounded history of canonical snapshot bytes and builds every delta
//! against the exact `(tick, hash)` baseline most recently acknowledged by that
//! client. A client retains its own bounded baseline history so loss of an
//! intermediate state packet does not make a newer packet depend on it.
//!
//! This module does not schedule packets or initiate the reliable resync
//! transfer. Instead, missing baselines and patches that cannot fit the bounded
//! state datagram produce a typed [`FullResyncRequired`] value for the session
//! layer. All network reconstruction happens in one fixed 128 KiB scratch buffer
//! before decoded state is exposed to rollback.

use std::collections::VecDeque;
use std::error::Error;
use std::fmt;

use crate::network_codec::{ProcessedInputAck, StateDeltaAndAcks};
use crate::network_protocol::{
    InputBatch, MAX_RESYNC_SNAPSHOT_BYTES, MAX_SEATS, MatchId, PeerId, ProtocolValidationError,
    ResyncApplied, StateBaselineAck, StateHash,
};
use crate::simulation::SimTick;
use crate::snapshot::{CanonicalSnapshot, SnapshotError, hash_canonical_bytes};
#[cfg(test)]
use crate::state_delta::MAX_STATE_DELTA_BYTES;
use crate::state_delta::{DeltaApplyError, DeltaBuildError, SnapshotByteDelta};

/// The state history covers normal acknowledgement and packet-loss windows while
/// keeping its worst-case memory use explicit: 64 entries * 128 KiB per endpoint.
pub const MIN_STATE_SYNC_HISTORY_ENTRIES: usize = 2;
pub const MAX_STATE_SYNC_HISTORY_ENTRIES: usize = 64;
pub const DEFAULT_STATE_SYNC_HISTORY_ENTRIES: usize = 32;

/// At most one remote peer can own each of the four fighter seats. A peer may
/// own multiple seats, so this remains a peer bound rather than a seat registry.
pub const MAX_STATE_SYNC_PEERS: usize = MAX_SEATS;

/// The exact canonical state identity acknowledged by a client.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct StateBaseline {
    pub tick: SimTick,
    pub hash: StateHash,
}

impl StateBaseline {
    pub const fn new(tick: SimTick, hash: StateHash) -> Self {
        Self { tick, hash }
    }
}

impl From<StateBaseline> for StateBaselineAck {
    fn from(value: StateBaseline) -> Self {
        Self {
            tick: value.tick,
            hash: value.hash,
        }
    }
}

impl From<StateBaselineAck> for StateBaseline {
    fn from(value: StateBaselineAck) -> Self {
        Self::new(value.tick, value.hash)
    }
}

/// Shared failures while accepting canonical bytes into either bounded history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StateSyncError {
    Protocol(ProtocolValidationError),
    Snapshot(SnapshotError),
    InvalidHistoryCapacity {
        requested: usize,
        min: usize,
        max: usize,
    },
    SnapshotTooLarge {
        bytes: usize,
        maximum: usize,
    },
    SnapshotMatchMismatch,
    SnapshotTickMismatch {
        expected: SimTick,
        actual: SimTick,
    },
    SnapshotHashMismatch {
        expected: StateHash,
        actual: StateHash,
    },
    ConflictingSnapshotAtTick {
        tick: SimTick,
        retained_hash: StateHash,
        offered_hash: StateHash,
    },
    StaleSnapshot {
        latest: SimTick,
        offered: SimTick,
    },
    StoredSnapshotHashMismatch {
        tick: SimTick,
        expected: StateHash,
        actual: StateHash,
    },
    NoAuthoritySnapshot,
    DeltaBuild(DeltaBuildError),
}

impl fmt::Display for StateSyncError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Protocol(error) => {
                write!(formatter, "invalid state-sync protocol value: {error}")
            }
            Self::Snapshot(error) => write!(formatter, "invalid canonical snapshot: {error}"),
            Self::InvalidHistoryCapacity {
                requested,
                min,
                max,
            } => write!(
                formatter,
                "state-sync history capacity {requested} is outside {min}..={max}"
            ),
            Self::SnapshotTooLarge { bytes, maximum } => write!(
                formatter,
                "state-sync snapshot has {bytes} bytes; maximum is {maximum}"
            ),
            Self::SnapshotMatchMismatch => {
                write!(formatter, "canonical snapshot belongs to another match")
            }
            Self::SnapshotTickMismatch { expected, actual } => write!(
                formatter,
                "canonical snapshot tick {} differs from declared tick {}",
                actual.get(),
                expected.get()
            ),
            Self::SnapshotHashMismatch { expected, actual } => write!(
                formatter,
                "canonical snapshot hash {:#018x} differs from declared hash {:#018x}",
                actual.0, expected.0
            ),
            Self::ConflictingSnapshotAtTick {
                tick,
                retained_hash,
                offered_hash,
            } => write!(
                formatter,
                "snapshot tick {} conflicts: retained {:#018x}, offered {:#018x}",
                tick.get(),
                retained_hash.0,
                offered_hash.0
            ),
            Self::StaleSnapshot { latest, offered } => write!(
                formatter,
                "snapshot tick {} is older than retained latest tick {}",
                offered.get(),
                latest.get()
            ),
            Self::StoredSnapshotHashMismatch {
                tick,
                expected,
                actual,
            } => write!(
                formatter,
                "stored snapshot tick {} hashes to {:#018x}, expected {:#018x}",
                tick.get(),
                actual.0,
                expected.0
            ),
            Self::NoAuthoritySnapshot => write!(formatter, "authority has no snapshot to publish"),
            Self::DeltaBuild(error) => write!(formatter, "state delta build failed: {error:?}"),
        }
    }
}

impl Error for StateSyncError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Protocol(error) => Some(error),
            Self::Snapshot(error) => Some(error),
            _ => None,
        }
    }
}

impl From<ProtocolValidationError> for StateSyncError {
    fn from(value: ProtocolValidationError) -> Self {
        Self::Protocol(value)
    }
}

impl From<SnapshotError> for StateSyncError {
    fn from(value: SnapshotError) -> Self {
        Self::Snapshot(value)
    }
}

struct StoredSnapshotBytes {
    baseline: StateBaseline,
    bytes: Box<[u8]>,
}

impl StoredSnapshotBytes {
    fn verify_hash(&self) -> Result<(), StateSyncError> {
        if self.bytes.len() > MAX_RESYNC_SNAPSHOT_BYTES {
            return Err(StateSyncError::SnapshotTooLarge {
                bytes: self.bytes.len(),
                maximum: MAX_RESYNC_SNAPSHOT_BYTES,
            });
        }
        let actual = StateHash(hash_canonical_bytes(&self.bytes));
        if actual != self.baseline.hash {
            return Err(StateSyncError::StoredSnapshotHashMismatch {
                tick: self.baseline.tick,
                expected: self.baseline.hash,
                actual,
            });
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HistoryStoreOutcome {
    Stored { evicted: Option<StateBaseline> },
    Duplicate,
}

/// Internal bounded ring shared by authority and client policy wrappers.
struct SnapshotByteHistory {
    match_id: MatchId,
    capacity: usize,
    entries: VecDeque<StoredSnapshotBytes>,
}

impl SnapshotByteHistory {
    fn new(match_id: MatchId, capacity: usize) -> Result<Self, StateSyncError> {
        match_id.validate()?;
        if !(MIN_STATE_SYNC_HISTORY_ENTRIES..=MAX_STATE_SYNC_HISTORY_ENTRIES).contains(&capacity) {
            return Err(StateSyncError::InvalidHistoryCapacity {
                requested: capacity,
                min: MIN_STATE_SYNC_HISTORY_ENTRIES,
                max: MAX_STATE_SYNC_HISTORY_ENTRIES,
            });
        }
        Ok(Self {
            match_id,
            capacity,
            entries: VecDeque::with_capacity(capacity),
        })
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn latest(&self) -> Option<&StoredSnapshotBytes> {
        self.entries.back()
    }

    fn find_tick(&self, tick: SimTick) -> Option<&StoredSnapshotBytes> {
        self.entries
            .iter()
            .rev()
            .find(|snapshot| snapshot.baseline.tick == tick)
    }

    fn insert_prevalidated(
        &mut self,
        baseline: StateBaseline,
        bytes: &[u8],
    ) -> Result<HistoryStoreOutcome, StateSyncError> {
        debug_assert!(!bytes.is_empty());
        debug_assert!(bytes.len() <= MAX_RESYNC_SNAPSHOT_BYTES);

        if let Some(existing) = self.find_tick(baseline.tick) {
            if existing.baseline == baseline && existing.bytes.as_ref() == bytes {
                return Ok(HistoryStoreOutcome::Duplicate);
            }
            return Err(StateSyncError::ConflictingSnapshotAtTick {
                tick: baseline.tick,
                retained_hash: existing.baseline.hash,
                offered_hash: baseline.hash,
            });
        }
        if let Some(latest) = self.latest()
            && baseline.tick < latest.baseline.tick
        {
            return Err(StateSyncError::StaleSnapshot {
                latest: latest.baseline.tick,
                offered: baseline.tick,
            });
        }

        let evicted = if self.entries.len() == self.capacity {
            self.entries.pop_front().map(|entry| entry.baseline)
        } else {
            None
        };
        self.entries.push_back(StoredSnapshotBytes {
            baseline,
            bytes: bytes.to_vec().into_boxed_slice(),
        });
        Ok(HistoryStoreOutcome::Stored { evicted })
    }

    fn clear(&mut self) {
        self.entries.clear();
    }
}

fn encoded_snapshot_identity(
    match_id: MatchId,
    snapshot: &CanonicalSnapshot,
) -> Result<(StateBaseline, Vec<u8>), StateSyncError> {
    if snapshot.header.match_id != *match_id.as_bytes() {
        return Err(StateSyncError::SnapshotMatchMismatch);
    }
    let bytes = snapshot.encode()?;
    validate_network_size(bytes.len())?;
    let baseline = StateBaseline::new(
        snapshot.header.tick,
        StateHash(hash_canonical_bytes(&bytes)),
    );
    Ok((baseline, bytes))
}

fn decode_verified_snapshot(
    match_id: MatchId,
    expected: StateBaseline,
    bytes: &[u8],
) -> Result<CanonicalSnapshot, StateSyncError> {
    validate_network_size(bytes.len())?;
    let actual_hash = StateHash(hash_canonical_bytes(bytes));
    if actual_hash != expected.hash {
        return Err(StateSyncError::SnapshotHashMismatch {
            expected: expected.hash,
            actual: actual_hash,
        });
    }
    let snapshot = CanonicalSnapshot::decode(bytes)?;
    if snapshot.header.match_id != *match_id.as_bytes() {
        return Err(StateSyncError::SnapshotMatchMismatch);
    }
    if snapshot.header.tick != expected.tick {
        return Err(StateSyncError::SnapshotTickMismatch {
            expected: expected.tick,
            actual: snapshot.header.tick,
        });
    }
    let decoded_hash = StateHash(snapshot.canonical_hash()?);
    if decoded_hash != expected.hash {
        return Err(StateSyncError::SnapshotHashMismatch {
            expected: expected.hash,
            actual: decoded_hash,
        });
    }
    Ok(snapshot)
}

fn validate_network_size(bytes: usize) -> Result<(), StateSyncError> {
    if bytes == 0 || bytes > MAX_RESYNC_SNAPSHOT_BYTES {
        Err(StateSyncError::SnapshotTooLarge {
            bytes,
            maximum: MAX_RESYNC_SNAPSHOT_BYTES,
        })
    } else {
        Ok(())
    }
}

fn increment(counter: &mut u64) {
    *counter = counter.saturating_add(1);
}

fn add(counter: &mut u64, value: usize) {
    *counter = counter.saturating_add(value as u64);
}

/// Non-canonical authority-side operational counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AuthorityStateSyncMetrics {
    pub snapshots_stored: u64,
    pub snapshot_bytes_stored: u64,
    pub snapshots_evicted: u64,
    pub duplicate_snapshots: u64,
    pub deltas_built: u64,
    pub delta_payload_bytes: u64,
    pub full_resync_baseline_missing: u64,
    pub full_resync_baseline_hash_mismatch: u64,
    pub full_resync_dense_delta: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FullResyncReason {
    BaselineMissing,
    BaselineHashMismatch { retained_hash: StateHash },
    DeltaTooDense,
}

/// Session-layer instruction to switch from unreliable state to reliable resync.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FullResyncRequired {
    pub reason: FullResyncReason,
    pub acknowledged: StateBaseline,
    pub target: StateBaseline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthorityDeltaOutcome {
    Delta(StateDeltaAndAcks),
    FullResyncRequired(FullResyncRequired),
}

/// Bounded canonical-byte history owned by one authority match instance.
pub struct AuthoritySnapshotHistory {
    history: SnapshotByteHistory,
    metrics: AuthorityStateSyncMetrics,
}

impl AuthoritySnapshotHistory {
    pub fn new(match_id: MatchId, capacity: usize) -> Result<Self, StateSyncError> {
        Ok(Self {
            history: SnapshotByteHistory::new(match_id, capacity)?,
            metrics: AuthorityStateSyncMetrics::default(),
        })
    }

    pub fn with_default_capacity(match_id: MatchId) -> Result<Self, StateSyncError> {
        Self::new(match_id, DEFAULT_STATE_SYNC_HISTORY_ENTRIES)
    }

    pub const fn metrics(&self) -> &AuthorityStateSyncMetrics {
        &self.metrics
    }

    pub fn len(&self) -> usize {
        self.history.len()
    }

    pub fn latest_baseline(&self) -> Option<StateBaseline> {
        self.history.latest().map(|snapshot| snapshot.baseline)
    }

    /// Returns the authority-authored identity for a tick that is still inside
    /// the bounded delta history. `None` means the tick has expired (or has not
    /// happened); callers must compare against [`Self::latest_baseline`] first
    /// to distinguish those cases.
    pub fn retained_baseline_at(&self, tick: SimTick) -> Option<StateBaseline> {
        self.history
            .find_tick(tick)
            .map(|snapshot| snapshot.baseline)
    }

    /// Encodes, validates, hashes, and stores one completed authoritative tick.
    pub fn record_snapshot(
        &mut self,
        snapshot: &CanonicalSnapshot,
    ) -> Result<StateBaseline, StateSyncError> {
        let (baseline, bytes) = encoded_snapshot_identity(self.history.match_id, snapshot)?;
        self.store_prevalidated(baseline, &bytes)?;
        Ok(baseline)
    }

    /// Stores externally encoded canonical bytes after size, hash, match, tick,
    /// decode, and canonical re-encode verification.
    pub fn record_encoded(
        &mut self,
        expected: StateBaseline,
        bytes: &[u8],
    ) -> Result<StateBaseline, StateSyncError> {
        decode_verified_snapshot(self.history.match_id, expected, bytes)?;
        self.store_prevalidated(expected, bytes)?;
        Ok(expected)
    }

    fn store_prevalidated(
        &mut self,
        baseline: StateBaseline,
        bytes: &[u8],
    ) -> Result<(), StateSyncError> {
        match self.history.insert_prevalidated(baseline, bytes)? {
            HistoryStoreOutcome::Stored { evicted } => {
                increment(&mut self.metrics.snapshots_stored);
                add(&mut self.metrics.snapshot_bytes_stored, bytes.len());
                if evicted.is_some() {
                    increment(&mut self.metrics.snapshots_evicted);
                }
            }
            HistoryStoreOutcome::Duplicate => {
                increment(&mut self.metrics.duplicate_snapshots);
            }
        }
        Ok(())
    }

    /// Builds the newest available state against the client's explicitly
    /// acknowledged baseline. No implicit "previous packet" baseline exists.
    pub fn build_latest_delta(
        &mut self,
        acknowledged: StateBaseline,
        acks: &[ProcessedInputAck],
    ) -> Result<AuthorityDeltaOutcome, StateSyncError> {
        let Some(target) = self.history.latest() else {
            return Err(StateSyncError::NoAuthoritySnapshot);
        };
        target.verify_hash()?;
        let target_baseline = target.baseline;

        let Some(base) = self.history.find_tick(acknowledged.tick) else {
            increment(&mut self.metrics.full_resync_baseline_missing);
            return Ok(AuthorityDeltaOutcome::FullResyncRequired(
                FullResyncRequired {
                    reason: FullResyncReason::BaselineMissing,
                    acknowledged,
                    target: target_baseline,
                },
            ));
        };
        if base.baseline.hash != acknowledged.hash {
            let retained_hash = base.baseline.hash;
            increment(&mut self.metrics.full_resync_baseline_hash_mismatch);
            return Ok(AuthorityDeltaOutcome::FullResyncRequired(
                FullResyncRequired {
                    reason: FullResyncReason::BaselineHashMismatch { retained_hash },
                    acknowledged,
                    target: target_baseline,
                },
            ));
        }
        base.verify_hash()?;

        let delta = match SnapshotByteDelta::from_canonical_bytes(&base.bytes, &target.bytes) {
            Ok(delta) => delta,
            Err(DeltaBuildError::PatchTooLarge { .. } | DeltaBuildError::TooManyRuns) => {
                increment(&mut self.metrics.full_resync_dense_delta);
                return Ok(AuthorityDeltaOutcome::FullResyncRequired(
                    FullResyncRequired {
                        reason: FullResyncReason::DeltaTooDense,
                        acknowledged,
                        target: target_baseline,
                    },
                ));
            }
            Err(error @ DeltaBuildError::SnapshotTooLarge { .. }) => {
                return Err(StateSyncError::DeltaBuild(error));
            }
        };
        let message = StateDeltaAndAcks::new(
            self.history.match_id,
            acknowledged.tick,
            acknowledged.hash,
            target_baseline.tick,
            target_baseline.hash,
            delta,
            acks,
        )?;
        increment(&mut self.metrics.deltas_built);
        add(&mut self.metrics.delta_payload_bytes, delta.payload_len());
        Ok(AuthorityDeltaOutcome::Delta(message))
    }
}

/// Failures at the connected-peer acknowledgement boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PeerStateSyncError {
    Protocol(ProtocolValidationError),
    InvalidPeerCapacity {
        requested: usize,
        maximum: usize,
    },
    PeerCapacityExceeded {
        capacity: usize,
    },
    MatchMismatch,
    PeerMismatch {
        connected: PeerId,
        claimed: PeerId,
    },
    UnknownPeer(PeerId),
    ConflictingAcknowledgement {
        peer_id: PeerId,
        tick: SimTick,
        retained: StateHash,
        offered: StateHash,
    },
    FutureAcknowledgement {
        peer_id: PeerId,
        latest: SimTick,
        offered: SimTick,
    },
    AuthorityHashMismatch {
        peer_id: PeerId,
        tick: SimTick,
        authority: StateHash,
        offered: StateHash,
    },
    AuthorityHistoryMatchMismatch,
    StateSync(StateSyncError),
}

impl fmt::Display for PeerStateSyncError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Protocol(error) => write!(formatter, "invalid peer state-sync value: {error}"),
            Self::InvalidPeerCapacity { requested, maximum } => write!(
                formatter,
                "peer state-sync capacity {requested} is outside 1..={maximum}"
            ),
            Self::PeerCapacityExceeded { capacity } => {
                write!(formatter, "peer state-sync registry is full at {capacity}")
            }
            Self::MatchMismatch => write!(formatter, "input batch belongs to another match"),
            Self::PeerMismatch { connected, claimed } => write!(
                formatter,
                "connected peer {} received input claiming peer {}",
                connected.get(),
                claimed.get()
            ),
            Self::UnknownPeer(peer_id) => {
                write!(formatter, "peer {} is not connected", peer_id.get())
            }
            Self::ConflictingAcknowledgement {
                peer_id,
                tick,
                retained,
                offered,
            } => write!(
                formatter,
                "peer {} acknowledged conflicting hashes for tick {}: retained {:#018x}, offered {:#018x}",
                peer_id.get(),
                tick.get(),
                retained.0,
                offered.0
            ),
            Self::FutureAcknowledgement {
                peer_id,
                latest,
                offered,
            } => write!(
                formatter,
                "peer {} acknowledged future tick {}; authority latest is {}",
                peer_id.get(),
                offered.get(),
                latest.get()
            ),
            Self::AuthorityHashMismatch {
                peer_id,
                tick,
                authority,
                offered,
            } => write!(
                formatter,
                "peer {} acknowledged non-authoritative hash {:#018x} for retained tick {}; authority hash is {:#018x}",
                peer_id.get(),
                offered.0,
                tick.get(),
                authority.0
            ),
            Self::AuthorityHistoryMatchMismatch => {
                write!(
                    formatter,
                    "authority snapshot history belongs to another match"
                )
            }
            Self::StateSync(error) => write!(formatter, "authority state-sync failed: {error}"),
        }
    }
}

impl Error for PeerStateSyncError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Protocol(error) => Some(error),
            Self::StateSync(error) => Some(error),
            _ => None,
        }
    }
}

impl From<ProtocolValidationError> for PeerStateSyncError {
    fn from(value: ProtocolValidationError) -> Self {
        Self::Protocol(value)
    }
}

impl From<StateSyncError> for PeerStateSyncError {
    fn from(value: StateSyncError) -> Self {
        Self::StateSync(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerRegistrationOutcome {
    Connected,
    AlreadyConnected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerBaselineAckOutcome {
    NoAcknowledgement {
        retained: Option<StateBaseline>,
    },
    Accepted {
        previous: Option<StateBaseline>,
        acknowledged: StateBaseline,
    },
    Duplicate(StateBaseline),
    IgnoredStale {
        retained: StateBaseline,
        offered: StateBaseline,
    },
    IgnoredExpired {
        retained: Option<StateBaseline>,
        offered: StateBaseline,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerStateUpdateOutcome {
    AwaitingBaselineAcknowledgement {
        peer_id: PeerId,
        target: StateBaseline,
    },
    Delta {
        peer_id: PeerId,
        message: StateDeltaAndAcks,
    },
    FullResyncRequired {
        peer_id: PeerId,
        required: FullResyncRequired,
    },
}

/// Non-canonical peer-registry operational counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PeerStateSyncMetrics {
    pub peers_connected: u64,
    pub duplicate_connects: u64,
    pub peers_disconnected: u64,
    pub batches_without_acknowledgement: u64,
    pub acknowledgements_accepted: u64,
    pub duplicate_acknowledgements: u64,
    pub stale_acknowledgements: u64,
    pub expired_acknowledgements: u64,
    pub conflicting_acknowledgements: u64,
    pub future_acknowledgements: u64,
    pub authority_hash_mismatches: u64,
    pub rejected_batches: u64,
    pub updates_awaiting_acknowledgement: u64,
    pub deltas_built: u64,
    pub full_resyncs_required: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PeerBaselineSlot {
    peer_id: PeerId,
    acknowledged: Option<StateBaseline>,
}

/// Fixed-capacity baseline registry for the remote peers in one match.
///
/// The input/ownership layer must call [`InputBatch::validate_for`] before this
/// coordinator. This boundary revalidates structure and checks the transport's
/// connected peer and match identity before observing the optional baseline ack;
/// it deliberately does not duplicate seat ownership or input-tick policy.
pub struct AuthorityStateSyncCoordinator {
    match_id: MatchId,
    capacity: usize,
    peer_count: usize,
    peers: [Option<PeerBaselineSlot>; MAX_STATE_SYNC_PEERS],
    metrics: PeerStateSyncMetrics,
}

impl AuthorityStateSyncCoordinator {
    pub fn new(match_id: MatchId, capacity: usize) -> Result<Self, PeerStateSyncError> {
        match_id.validate()?;
        if !(1..=MAX_STATE_SYNC_PEERS).contains(&capacity) {
            return Err(PeerStateSyncError::InvalidPeerCapacity {
                requested: capacity,
                maximum: MAX_STATE_SYNC_PEERS,
            });
        }
        Ok(Self {
            match_id,
            capacity,
            peer_count: 0,
            peers: [None; MAX_STATE_SYNC_PEERS],
            metrics: PeerStateSyncMetrics::default(),
        })
    }

    pub fn with_maximum_capacity(match_id: MatchId) -> Result<Self, PeerStateSyncError> {
        Self::new(match_id, MAX_STATE_SYNC_PEERS)
    }

    pub const fn metrics(&self) -> &PeerStateSyncMetrics {
        &self.metrics
    }

    pub const fn len(&self) -> usize {
        self.peer_count
    }

    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    pub const fn is_empty(&self) -> bool {
        self.peer_count == 0
    }

    fn peer_index(&self, peer_id: PeerId) -> Option<usize> {
        self.peers[..self.capacity]
            .iter()
            .position(|slot| slot.is_some_and(|slot| slot.peer_id == peer_id))
    }

    pub fn connect_peer(
        &mut self,
        peer_id: PeerId,
    ) -> Result<PeerRegistrationOutcome, PeerStateSyncError> {
        peer_id.validate()?;
        if self.peer_index(peer_id).is_some() {
            increment(&mut self.metrics.duplicate_connects);
            return Ok(PeerRegistrationOutcome::AlreadyConnected);
        }
        let Some(index) = self.peers[..self.capacity].iter().position(Option::is_none) else {
            return Err(PeerStateSyncError::PeerCapacityExceeded {
                capacity: self.capacity,
            });
        };
        self.peers[index] = Some(PeerBaselineSlot {
            peer_id,
            acknowledged: None,
        });
        self.peer_count += 1;
        increment(&mut self.metrics.peers_connected);
        Ok(PeerRegistrationOutcome::Connected)
    }

    pub fn disconnect_peer(&mut self, peer_id: PeerId) -> bool {
        let Some(index) = self.peer_index(peer_id) else {
            return false;
        };
        self.peers[index] = None;
        self.peer_count -= 1;
        increment(&mut self.metrics.peers_disconnected);
        true
    }

    pub fn acknowledged_baseline(
        &self,
        peer_id: PeerId,
    ) -> Result<Option<StateBaseline>, PeerStateSyncError> {
        let index = self
            .peer_index(peer_id)
            .ok_or(PeerStateSyncError::UnknownPeer(peer_id))?;
        Ok(self.peers[index]
            .expect("peer index always points to an occupied slot")
            .acknowledged)
    }

    /// Observes an input-batch acknowledgement only after proving that it
    /// cannot name authority state from the future and that any tick still in
    /// bounded authority history carries the exact authority-authored hash.
    ///
    /// A hash for an already-expired tick cannot be verified and therefore
    /// never advances or replaces the retained baseline. If the retained
    /// authority-verified baseline has itself expired, ordinary delta
    /// production selects the bounded full-resync path exactly once.
    pub fn observe_validated_input_batch(
        &mut self,
        connected_peer: PeerId,
        batch: &InputBatch,
        authority: &AuthoritySnapshotHistory,
    ) -> Result<PeerBaselineAckOutcome, PeerStateSyncError> {
        if let Err(error) = batch.validate_structure() {
            increment(&mut self.metrics.rejected_batches);
            return Err(PeerStateSyncError::Protocol(error));
        }
        if batch.match_id != self.match_id {
            increment(&mut self.metrics.rejected_batches);
            return Err(PeerStateSyncError::MatchMismatch);
        }
        if batch.peer_id != connected_peer {
            increment(&mut self.metrics.rejected_batches);
            return Err(PeerStateSyncError::PeerMismatch {
                connected: connected_peer,
                claimed: batch.peer_id,
            });
        }
        let Some(index) = self.peer_index(connected_peer) else {
            increment(&mut self.metrics.rejected_batches);
            return Err(PeerStateSyncError::UnknownPeer(connected_peer));
        };
        let slot = self.peers[index]
            .as_mut()
            .expect("peer index always points to an occupied slot");
        let Some(offered) = batch.state_baseline_ack().map(StateBaseline::from) else {
            increment(&mut self.metrics.batches_without_acknowledgement);
            return Ok(PeerBaselineAckOutcome::NoAcknowledgement {
                retained: slot.acknowledged,
            });
        };

        if authority.history.match_id != self.match_id {
            increment(&mut self.metrics.rejected_batches);
            return Err(PeerStateSyncError::AuthorityHistoryMatchMismatch);
        }
        let Some(latest) = authority.latest_baseline() else {
            increment(&mut self.metrics.rejected_batches);
            return Err(PeerStateSyncError::StateSync(
                StateSyncError::NoAuthoritySnapshot,
            ));
        };
        if offered.tick > latest.tick {
            increment(&mut self.metrics.future_acknowledgements);
            increment(&mut self.metrics.rejected_batches);
            return Err(PeerStateSyncError::FutureAcknowledgement {
                peer_id: connected_peer,
                latest: latest.tick,
                offered: offered.tick,
            });
        }
        match authority.retained_baseline_at(offered.tick) {
            Some(retained) if retained.hash != offered.hash => {
                increment(&mut self.metrics.authority_hash_mismatches);
                increment(&mut self.metrics.rejected_batches);
                return Err(PeerStateSyncError::AuthorityHashMismatch {
                    peer_id: connected_peer,
                    tick: offered.tick,
                    authority: retained.hash,
                    offered: offered.hash,
                });
            }
            Some(_) => {}
            None => {
                increment(&mut self.metrics.expired_acknowledgements);
                return Ok(PeerBaselineAckOutcome::IgnoredExpired {
                    retained: slot.acknowledged,
                    offered,
                });
            }
        }

        self.observe_baseline_at(index, offered)
    }

    /// Advances the peer's delta baseline immediately after the caller has
    /// successfully validated a [`ResyncApplied`] against its active
    /// [`crate::resync_transfer::AuthorityResyncTransfer`]. This closes the
    /// one-network-tick gap before the next input batch carries the same
    /// acknowledgement, preventing repeated full snapshots for one completed
    /// transfer. The transfer identity check remains the caller's responsibility.
    pub fn observe_validated_resync_applied(
        &mut self,
        connected_peer: PeerId,
        applied: &ResyncApplied,
    ) -> Result<PeerBaselineAckOutcome, PeerStateSyncError> {
        if let Err(error) = applied.validate() {
            increment(&mut self.metrics.rejected_batches);
            return Err(PeerStateSyncError::Protocol(error));
        }
        if applied.match_id != self.match_id {
            increment(&mut self.metrics.rejected_batches);
            return Err(PeerStateSyncError::MatchMismatch);
        }
        if applied.peer_id != connected_peer {
            increment(&mut self.metrics.rejected_batches);
            return Err(PeerStateSyncError::PeerMismatch {
                connected: connected_peer,
                claimed: applied.peer_id,
            });
        }
        let index = self.peer_index(connected_peer).ok_or_else(|| {
            increment(&mut self.metrics.rejected_batches);
            PeerStateSyncError::UnknownPeer(connected_peer)
        })?;
        self.replace_baseline_at(
            index,
            StateBaseline::new(applied.snapshot_tick, applied.snapshot_hash),
        )
    }

    fn replace_baseline_at(
        &mut self,
        index: usize,
        offered: StateBaseline,
    ) -> Result<PeerBaselineAckOutcome, PeerStateSyncError> {
        let slot = self.peers[index]
            .as_mut()
            .expect("peer index always points to an occupied slot");
        let previous = slot.acknowledged;
        if previous == Some(offered) {
            increment(&mut self.metrics.duplicate_acknowledgements);
            return Ok(PeerBaselineAckOutcome::Duplicate(offered));
        }
        slot.acknowledged = Some(offered);
        increment(&mut self.metrics.acknowledgements_accepted);
        Ok(PeerBaselineAckOutcome::Accepted {
            previous,
            acknowledged: offered,
        })
    }

    fn observe_baseline_at(
        &mut self,
        index: usize,
        offered: StateBaseline,
    ) -> Result<PeerBaselineAckOutcome, PeerStateSyncError> {
        let slot = self.peers[index]
            .as_mut()
            .expect("peer index always points to an occupied slot");
        let connected_peer = slot.peer_id;

        let Some(retained) = slot.acknowledged else {
            slot.acknowledged = Some(offered);
            increment(&mut self.metrics.acknowledgements_accepted);
            return Ok(PeerBaselineAckOutcome::Accepted {
                previous: None,
                acknowledged: offered,
            });
        };
        if offered.tick < retained.tick {
            increment(&mut self.metrics.stale_acknowledgements);
            return Ok(PeerBaselineAckOutcome::IgnoredStale { retained, offered });
        }
        if offered.tick == retained.tick {
            if offered.hash == retained.hash {
                increment(&mut self.metrics.duplicate_acknowledgements);
                return Ok(PeerBaselineAckOutcome::Duplicate(retained));
            }
            increment(&mut self.metrics.conflicting_acknowledgements);
            increment(&mut self.metrics.rejected_batches);
            return Err(PeerStateSyncError::ConflictingAcknowledgement {
                peer_id: connected_peer,
                tick: offered.tick,
                retained: retained.hash,
                offered: offered.hash,
            });
        }

        slot.acknowledged = Some(offered);
        increment(&mut self.metrics.acknowledgements_accepted);
        Ok(PeerBaselineAckOutcome::Accepted {
            previous: Some(retained),
            acknowledged: offered,
        })
    }

    /// Builds the newest state packet using only this peer's retained explicit
    /// acknowledgement. Before the first acknowledgement the caller receives a
    /// typed waiting outcome and should keep initial/full resync state in flight.
    pub fn build_latest_for_peer(
        &mut self,
        authority: &mut AuthoritySnapshotHistory,
        peer_id: PeerId,
        acks: &[ProcessedInputAck],
    ) -> Result<PeerStateUpdateOutcome, PeerStateSyncError> {
        if authority.history.match_id != self.match_id {
            return Err(PeerStateSyncError::AuthorityHistoryMatchMismatch);
        }
        let acknowledged = self.acknowledged_baseline(peer_id)?;
        let Some(acknowledged) = acknowledged else {
            let target = authority
                .latest_baseline()
                .ok_or(StateSyncError::NoAuthoritySnapshot)?;
            increment(&mut self.metrics.updates_awaiting_acknowledgement);
            return Ok(PeerStateUpdateOutcome::AwaitingBaselineAcknowledgement { peer_id, target });
        };

        match authority.build_latest_delta(acknowledged, acks)? {
            AuthorityDeltaOutcome::Delta(message) => {
                increment(&mut self.metrics.deltas_built);
                Ok(PeerStateUpdateOutcome::Delta { peer_id, message })
            }
            AuthorityDeltaOutcome::FullResyncRequired(required) => {
                increment(&mut self.metrics.full_resyncs_required);
                Ok(PeerStateUpdateOutcome::FullResyncRequired { peer_id, required })
            }
        }
    }
}

/// Client-side rejection reasons. Any error leaves the accepted baseline history
/// unchanged; only the private scratch bytes may have been overwritten.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientStateSyncError {
    Protocol(ProtocolValidationError),
    Storage(StateSyncError),
    MatchMismatch,
    BaselineMissing(StateBaseline),
    BaselineHashMismatch {
        tick: SimTick,
        declared: StateHash,
        retained: StateHash,
    },
    BaselineBytesHashMismatch {
        tick: SimTick,
        expected: StateHash,
        actual: StateHash,
    },
    DeltaApply(DeltaApplyError),
    TargetHashMismatch {
        expected: StateHash,
        actual: StateHash,
    },
    TargetSnapshot(SnapshotError),
    TargetMatchMismatch,
    TargetTickMismatch {
        expected: SimTick,
        actual: SimTick,
    },
    DecodedSnapshotHashMismatch {
        expected: StateHash,
        actual: StateHash,
    },
    ConflictingTargetTick {
        tick: SimTick,
        retained: StateHash,
        received: StateHash,
    },
}

impl fmt::Display for ClientStateSyncError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Protocol(error) => write!(formatter, "invalid state packet: {error}"),
            Self::Storage(error) => write!(formatter, "client baseline storage failed: {error}"),
            Self::MatchMismatch => write!(formatter, "state packet belongs to another match"),
            Self::BaselineMissing(baseline) => write!(
                formatter,
                "state baseline tick {} hash {:#018x} is not retained",
                baseline.tick.get(),
                baseline.hash.0
            ),
            Self::BaselineHashMismatch {
                tick,
                declared,
                retained,
            } => write!(
                formatter,
                "state baseline tick {} declares {:#018x}, retained {:#018x}",
                tick.get(),
                declared.0,
                retained.0
            ),
            Self::BaselineBytesHashMismatch {
                tick,
                expected,
                actual,
            } => write!(
                formatter,
                "retained baseline tick {} hashes to {:#018x}, expected {:#018x}",
                tick.get(),
                actual.0,
                expected.0
            ),
            Self::DeltaApply(error) => write!(formatter, "state patch is invalid: {error:?}"),
            Self::TargetHashMismatch { expected, actual } => write!(
                formatter,
                "patched target hashes to {:#018x}, expected {:#018x}",
                actual.0, expected.0
            ),
            Self::TargetSnapshot(error) => {
                write!(
                    formatter,
                    "patched target is not a canonical snapshot: {error}"
                )
            }
            Self::TargetMatchMismatch => {
                write!(
                    formatter,
                    "patched canonical snapshot belongs to another match"
                )
            }
            Self::TargetTickMismatch { expected, actual } => write!(
                formatter,
                "patched canonical snapshot tick {} differs from authority tick {}",
                actual.get(),
                expected.get()
            ),
            Self::DecodedSnapshotHashMismatch { expected, actual } => write!(
                formatter,
                "decoded canonical snapshot hashes to {:#018x}, expected {:#018x}",
                actual.0, expected.0
            ),
            Self::ConflictingTargetTick {
                tick,
                retained,
                received,
            } => write!(
                formatter,
                "received conflicting state for tick {}: retained {:#018x}, received {:#018x}",
                tick.get(),
                retained.0,
                received.0
            ),
        }
    }
}

impl Error for ClientStateSyncError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Protocol(error) => Some(error),
            Self::Storage(error) => Some(error),
            Self::TargetSnapshot(error) => Some(error),
            _ => None,
        }
    }
}

impl From<StateSyncError> for ClientStateSyncError {
    fn from(value: StateSyncError) -> Self {
        Self::Storage(value)
    }
}

/// Non-canonical client-side operational counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClientStateSyncMetrics {
    pub baselines_stored: u64,
    pub baseline_bytes_stored: u64,
    pub baselines_evicted: u64,
    pub duplicate_baselines: u64,
    pub baseline_resets: u64,
    pub deltas_applied: u64,
    pub target_bytes_applied: u64,
    pub duplicate_deltas: u64,
    pub stale_deltas_ignored: u64,
    pub baseline_misses: u64,
    pub baseline_hash_failures: u64,
    pub delta_apply_failures: u64,
    pub target_verification_failures: u64,
    pub rejected_messages: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppliedStateDelta {
    pub baseline: StateBaseline,
    pub snapshot: CanonicalSnapshot,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientDeltaOutcome {
    Applied(AppliedStateDelta),
    Duplicate(StateBaseline),
    IgnoredStale {
        received: StateBaseline,
        latest: StateBaseline,
    },
}

/// Bounded client baseline history plus one fixed 128 KiB patch destination.
pub struct ClientBaselineHistory {
    history: SnapshotByteHistory,
    apply_storage: Box<[u8]>,
    metrics: ClientStateSyncMetrics,
}

impl ClientBaselineHistory {
    pub fn new(match_id: MatchId, capacity: usize) -> Result<Self, StateSyncError> {
        Ok(Self {
            history: SnapshotByteHistory::new(match_id, capacity)?,
            apply_storage: vec![0; MAX_RESYNC_SNAPSHOT_BYTES].into_boxed_slice(),
            metrics: ClientStateSyncMetrics::default(),
        })
    }

    pub fn with_default_capacity(match_id: MatchId) -> Result<Self, StateSyncError> {
        Self::new(match_id, DEFAULT_STATE_SYNC_HISTORY_ENTRIES)
    }

    pub const fn metrics(&self) -> &ClientStateSyncMetrics {
        &self.metrics
    }

    pub fn len(&self) -> usize {
        self.history.len()
    }

    /// The pair to acknowledge to the authority for its next latest-wins delta.
    pub fn latest_baseline(&self) -> Option<StateBaseline> {
        self.history.latest().map(|snapshot| snapshot.baseline)
    }

    pub fn contains(&self, baseline: StateBaseline) -> bool {
        self.history
            .find_tick(baseline.tick)
            .is_some_and(|stored| stored.baseline == baseline)
    }

    /// Seeds history from a locally available full canonical snapshot.
    pub fn install_snapshot(
        &mut self,
        snapshot: &CanonicalSnapshot,
    ) -> Result<StateBaseline, StateSyncError> {
        let (baseline, bytes) = encoded_snapshot_identity(self.history.match_id, snapshot)?;
        self.store_prevalidated(baseline, &bytes)?;
        Ok(baseline)
    }

    /// Seeds history from full-resync bytes while independently checking every
    /// identity field supplied by the transfer metadata.
    pub fn install_encoded(
        &mut self,
        expected: StateBaseline,
        bytes: &[u8],
    ) -> Result<StateBaseline, StateSyncError> {
        decode_verified_snapshot(self.history.match_id, expected, bytes)?;
        self.store_prevalidated(expected, bytes)?;
        Ok(expected)
    }

    /// Replaces all delta baselines after a reliable hard correction. Validation
    /// happens before the old history is cleared.
    pub fn reset_to_snapshot(
        &mut self,
        snapshot: &CanonicalSnapshot,
    ) -> Result<StateBaseline, StateSyncError> {
        let (baseline, bytes) = encoded_snapshot_identity(self.history.match_id, snapshot)?;
        self.history.clear();
        self.store_prevalidated(baseline, &bytes)?;
        increment(&mut self.metrics.baseline_resets);
        Ok(baseline)
    }

    pub fn reset_to_encoded(
        &mut self,
        expected: StateBaseline,
        bytes: &[u8],
    ) -> Result<StateBaseline, StateSyncError> {
        decode_verified_snapshot(self.history.match_id, expected, bytes)?;
        self.history.clear();
        self.store_prevalidated(expected, bytes)?;
        increment(&mut self.metrics.baseline_resets);
        Ok(expected)
    }

    fn store_prevalidated(
        &mut self,
        baseline: StateBaseline,
        bytes: &[u8],
    ) -> Result<(), StateSyncError> {
        match self.history.insert_prevalidated(baseline, bytes)? {
            HistoryStoreOutcome::Stored { evicted } => {
                increment(&mut self.metrics.baselines_stored);
                add(&mut self.metrics.baseline_bytes_stored, bytes.len());
                if evicted.is_some() {
                    increment(&mut self.metrics.baselines_evicted);
                }
            }
            HistoryStoreOutcome::Duplicate => {
                increment(&mut self.metrics.duplicate_baselines);
            }
        }
        Ok(())
    }

    /// Applies one hostile latest-wins state message transactionally. The
    /// history changes only after patch, byte-hash, decode, match, tick, and
    /// decoded canonical-hash verification all succeed.
    pub fn apply_delta(
        &mut self,
        message: &StateDeltaAndAcks,
    ) -> Result<ClientDeltaOutcome, ClientStateSyncError> {
        if let Err(error) = message.validate() {
            increment(&mut self.metrics.rejected_messages);
            return Err(ClientStateSyncError::Protocol(error));
        }
        if message.match_id != self.history.match_id {
            increment(&mut self.metrics.rejected_messages);
            return Err(ClientStateSyncError::MatchMismatch);
        }

        let received = StateBaseline::new(message.authority_tick, message.state_hash);
        if let Some(latest) = self.latest_baseline() {
            if received.tick < latest.tick {
                increment(&mut self.metrics.stale_deltas_ignored);
                return Ok(ClientDeltaOutcome::IgnoredStale { received, latest });
            }
            if received.tick == latest.tick {
                if received.hash == latest.hash {
                    increment(&mut self.metrics.duplicate_deltas);
                    return Ok(ClientDeltaOutcome::Duplicate(latest));
                }
                increment(&mut self.metrics.target_verification_failures);
                increment(&mut self.metrics.rejected_messages);
                return Err(ClientStateSyncError::ConflictingTargetTick {
                    tick: received.tick,
                    retained: latest.hash,
                    received: received.hash,
                });
            }
        }

        let requested_base = StateBaseline::new(message.base_tick, message.base_hash);
        let Some(base) = self.history.find_tick(message.base_tick) else {
            increment(&mut self.metrics.baseline_misses);
            increment(&mut self.metrics.rejected_messages);
            return Err(ClientStateSyncError::BaselineMissing(requested_base));
        };
        if base.baseline.hash != message.base_hash {
            increment(&mut self.metrics.baseline_hash_failures);
            increment(&mut self.metrics.rejected_messages);
            return Err(ClientStateSyncError::BaselineHashMismatch {
                tick: message.base_tick,
                declared: message.base_hash,
                retained: base.baseline.hash,
            });
        }
        let actual_base_hash = StateHash(hash_canonical_bytes(&base.bytes));
        if actual_base_hash != message.base_hash {
            increment(&mut self.metrics.baseline_hash_failures);
            increment(&mut self.metrics.rejected_messages);
            return Err(ClientStateSyncError::BaselineBytesHashMismatch {
                tick: message.base_tick,
                expected: message.base_hash,
                actual: actual_base_hash,
            });
        }

        let target_len = match message.delta.apply(&base.bytes, &mut self.apply_storage) {
            Ok(length) => length,
            Err(error) => {
                increment(&mut self.metrics.delta_apply_failures);
                increment(&mut self.metrics.rejected_messages);
                return Err(ClientStateSyncError::DeltaApply(error));
            }
        };
        let target_bytes = &self.apply_storage[..target_len];
        let actual_target_hash = StateHash(hash_canonical_bytes(target_bytes));
        if actual_target_hash != message.state_hash {
            increment(&mut self.metrics.target_verification_failures);
            increment(&mut self.metrics.rejected_messages);
            return Err(ClientStateSyncError::TargetHashMismatch {
                expected: message.state_hash,
                actual: actual_target_hash,
            });
        }
        let snapshot = match CanonicalSnapshot::decode(target_bytes) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                increment(&mut self.metrics.target_verification_failures);
                increment(&mut self.metrics.rejected_messages);
                return Err(ClientStateSyncError::TargetSnapshot(error));
            }
        };
        if snapshot.header.match_id != *self.history.match_id.as_bytes() {
            increment(&mut self.metrics.target_verification_failures);
            increment(&mut self.metrics.rejected_messages);
            return Err(ClientStateSyncError::TargetMatchMismatch);
        }
        if snapshot.header.tick != message.authority_tick {
            increment(&mut self.metrics.target_verification_failures);
            increment(&mut self.metrics.rejected_messages);
            return Err(ClientStateSyncError::TargetTickMismatch {
                expected: message.authority_tick,
                actual: snapshot.header.tick,
            });
        }
        let decoded_hash = match snapshot.canonical_hash() {
            Ok(hash) => StateHash(hash),
            Err(error) => {
                increment(&mut self.metrics.target_verification_failures);
                increment(&mut self.metrics.rejected_messages);
                return Err(ClientStateSyncError::TargetSnapshot(error));
            }
        };
        if decoded_hash != message.state_hash {
            increment(&mut self.metrics.target_verification_failures);
            increment(&mut self.metrics.rejected_messages);
            return Err(ClientStateSyncError::DecodedSnapshotHashMismatch {
                expected: message.state_hash,
                actual: decoded_hash,
            });
        }

        // Copy only after all fallible network verification is complete.
        let accepted_bytes = target_bytes.to_vec();
        self.store_prevalidated(received, &accepted_bytes)?;
        increment(&mut self.metrics.deltas_applied);
        add(&mut self.metrics.target_bytes_applied, target_len);
        Ok(ClientDeltaOutcome::Applied(AppliedStateDelta {
            baseline: received,
            snapshot,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::determinism::{FighterId, RngStreamName, SimEntityKind};
    use crate::network_protocol::{
        InputFrame, InputSequence, InputTickWindow, SeatAssignment, SeatId, SeatInputWindow,
        SeatOwner, SeatOwnership, TransferId,
    };
    use crate::snapshot::{
        ArenaRuntimeSnapshot, FighterSnapshot, MatchStateSnapshot, MatchStatsSnapshot,
        NamedRngSnapshot, PoolAllocatorSnapshot, SIM_ENTITY_KIND_COUNT, SnapshotHeader,
    };

    const MATCH_BYTES: [u8; 16] = *b"state-sync-test1";

    fn match_id() -> MatchId {
        MatchId::new(MATCH_BYTES).unwrap()
    }

    fn peer_id(value: u64) -> PeerId {
        PeerId::new(value).unwrap()
    }

    fn input_batch(
        match_id: MatchId,
        peer_id: PeerId,
        seats: &[u8],
        acknowledgement: Option<StateBaseline>,
    ) -> InputBatch {
        let windows = seats
            .iter()
            .map(|seat| {
                SeatInputWindow::from_newest_first(&[InputFrame {
                    tick: SimTick(100),
                    seat: SeatId::new(*seat).unwrap(),
                    sequence: InputSequence(10),
                    ..InputFrame::default()
                }])
                .unwrap()
            })
            .collect::<Vec<_>>();
        let batch = InputBatch::new(match_id, peer_id, &windows).unwrap();
        acknowledgement
            .map(|acknowledgement| {
                batch
                    .with_state_baseline_ack(acknowledgement.into())
                    .unwrap()
            })
            .unwrap_or(batch)
    }

    fn fixture(tick: u64) -> CanonicalSnapshot {
        let allocators = SimEntityKind::ALL
            .into_iter()
            .map(|kind| PoolAllocatorSnapshot::empty(kind, 1).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(allocators.len(), SIM_ENTITY_KIND_COUNT);
        CanonicalSnapshot {
            header: SnapshotHeader::new(1, 1, 99, MATCH_BYTES, SimTick(tick), 123),
            match_state: MatchStateSnapshot::default(),
            fighters: FighterId::ALL.map(FighterSnapshot::empty),
            arena: ArenaRuntimeSnapshot::default(),
            allocators,
            dynamic_objects: Vec::new(),
            rng_streams: Vec::new(),
            stats: MatchStatsSnapshot::default(),
        }
    }

    fn dense_fixture(tick: u64) -> CanonicalSnapshot {
        let mut snapshot = fixture(tick);
        snapshot.rng_streams = (1..=64)
            .map(|code| {
                NamedRngSnapshot::new(
                    RngStreamName::from_code(code),
                    0xA5A5_0000_0000_0000 | code,
                    0x5A5A_FFFF_FFFF_FFFF ^ code,
                )
            })
            .collect();
        snapshot
    }

    fn expect_delta(outcome: AuthorityDeltaOutcome) -> StateDeltaAndAcks {
        match outcome {
            AuthorityDeltaOutcome::Delta(message) => message,
            AuthorityDeltaOutcome::FullResyncRequired(reason) => {
                panic!("expected delta, got {reason:?}")
            }
        }
    }

    #[test]
    fn lost_intermediate_packet_does_not_become_an_implicit_baseline() {
        let mut authority = AuthoritySnapshotHistory::new(match_id(), 4).unwrap();
        let tick_10 = fixture(10);
        let baseline_10 = authority.record_snapshot(&tick_10).unwrap();
        authority.record_snapshot(&fixture(11)).unwrap();
        let lost = expect_delta(authority.build_latest_delta(baseline_10, &[]).unwrap());
        assert_eq!(lost.authority_tick, SimTick(11));

        let tick_12 = fixture(12);
        let baseline_12 = authority.record_snapshot(&tick_12).unwrap();
        let newest = expect_delta(authority.build_latest_delta(baseline_10, &[]).unwrap());
        assert_eq!(newest.base_tick, SimTick(10));
        assert_eq!(newest.authority_tick, SimTick(12));

        let mut client = ClientBaselineHistory::new(match_id(), 4).unwrap();
        client.install_snapshot(&tick_10).unwrap();
        let applied = client.apply_delta(&newest).unwrap();
        assert!(matches!(
            applied,
            ClientDeltaOutcome::Applied(AppliedStateDelta {
                baseline,
                snapshot
            }) if baseline == baseline_12 && snapshot == tick_12
        ));
        assert_eq!(client.latest_baseline(), Some(baseline_12));

        // A delayed older packet cannot rewind the latest-wins client.
        assert!(matches!(
            client.apply_delta(&lost).unwrap(),
            ClientDeltaOutcome::IgnoredStale { received, latest }
                if received.tick == SimTick(11) && latest == baseline_12
        ));
        assert_eq!(client.metrics().deltas_applied, 1);
        assert_eq!(client.metrics().stale_deltas_ignored, 1);
    }

    #[test]
    fn evicted_or_wrong_acknowledged_baseline_requires_typed_full_resync() {
        let mut authority = AuthoritySnapshotHistory::new(match_id(), 2).unwrap();
        let baseline_1 = authority.record_snapshot(&fixture(1)).unwrap();
        let baseline_2 = authority.record_snapshot(&fixture(2)).unwrap();
        let baseline_3 = authority.record_snapshot(&fixture(3)).unwrap();
        assert_eq!(authority.len(), 2);

        assert_eq!(
            authority.build_latest_delta(baseline_1, &[]).unwrap(),
            AuthorityDeltaOutcome::FullResyncRequired(FullResyncRequired {
                reason: FullResyncReason::BaselineMissing,
                acknowledged: baseline_1,
                target: baseline_3,
            })
        );

        let wrong = StateBaseline::new(baseline_2.tick, StateHash(baseline_2.hash.0 ^ 1));
        assert_eq!(
            authority.build_latest_delta(wrong, &[]).unwrap(),
            AuthorityDeltaOutcome::FullResyncRequired(FullResyncRequired {
                reason: FullResyncReason::BaselineHashMismatch {
                    retained_hash: baseline_2.hash,
                },
                acknowledged: wrong,
                target: baseline_3,
            })
        );
        assert_eq!(authority.metrics().snapshots_evicted, 1);
        assert_eq!(authority.metrics().full_resync_baseline_missing, 1);
        assert_eq!(authority.metrics().full_resync_baseline_hash_mismatch, 1);
    }

    #[test]
    fn dense_patch_falls_back_without_emitting_an_oversized_state_message() {
        let mut authority = AuthoritySnapshotHistory::new(match_id(), 2).unwrap();
        let base = authority.record_snapshot(&fixture(20)).unwrap();
        let target = authority.record_snapshot(&dense_fixture(21)).unwrap();
        assert_eq!(
            authority.build_latest_delta(base, &[]).unwrap(),
            AuthorityDeltaOutcome::FullResyncRequired(FullResyncRequired {
                reason: FullResyncReason::DeltaTooDense,
                acknowledged: base,
                target,
            })
        );
        assert_eq!(authority.metrics().deltas_built, 0);
        assert_eq!(authority.metrics().full_resync_dense_delta, 1);
    }

    #[test]
    fn client_rejects_corrupt_target_hash_without_advancing_history() {
        let base_snapshot = fixture(30);
        let target_snapshot = fixture(31);
        let mut authority = AuthoritySnapshotHistory::new(match_id(), 2).unwrap();
        let base = authority.record_snapshot(&base_snapshot).unwrap();
        authority.record_snapshot(&target_snapshot).unwrap();
        let mut message = expect_delta(authority.build_latest_delta(base, &[]).unwrap());
        message.state_hash.0 ^= 1;

        let mut client = ClientBaselineHistory::new(match_id(), 2).unwrap();
        client.install_snapshot(&base_snapshot).unwrap();
        assert!(matches!(
            client.apply_delta(&message),
            Err(ClientStateSyncError::TargetHashMismatch { .. })
        ));
        assert_eq!(client.latest_baseline(), Some(base));
        assert_eq!(client.metrics().target_verification_failures, 1);
        assert_eq!(client.metrics().rejected_messages, 1);
    }

    #[test]
    fn client_verifies_retained_baseline_bytes_and_decoded_target_tick() {
        let base_snapshot = fixture(40);
        let declared_target = fixture(41);
        let wrong_tick_target = fixture(99);
        let base_bytes = base_snapshot.encode().unwrap();
        let wrong_bytes = wrong_tick_target.encode().unwrap();
        let base = StateBaseline::new(SimTick(40), StateHash(hash_canonical_bytes(&base_bytes)));
        let wrong_hash = StateHash(hash_canonical_bytes(&wrong_bytes));
        let delta = SnapshotByteDelta::from_canonical_bytes(&base_bytes, &wrong_bytes).unwrap();
        let message = StateDeltaAndAcks::new(
            match_id(),
            base.tick,
            base.hash,
            declared_target.header.tick,
            wrong_hash,
            delta,
            &[],
        )
        .unwrap();

        let mut tick_client = ClientBaselineHistory::new(match_id(), 2).unwrap();
        tick_client.install_snapshot(&base_snapshot).unwrap();
        assert_eq!(
            tick_client.apply_delta(&message),
            Err(ClientStateSyncError::TargetTickMismatch {
                expected: SimTick(41),
                actual: SimTick(99),
            })
        );
        assert_eq!(tick_client.latest_baseline(), Some(base));

        let mut corrupt_base_client = ClientBaselineHistory::new(match_id(), 2).unwrap();
        corrupt_base_client
            .install_snapshot(&base_snapshot)
            .unwrap();
        corrupt_base_client.history.entries[0].bytes[0] ^= 1;
        assert!(matches!(
            corrupt_base_client.apply_delta(&message),
            Err(ClientStateSyncError::BaselineBytesHashMismatch { tick, .. })
                if tick == SimTick(40)
        ));
        assert_eq!(corrupt_base_client.metrics().baseline_hash_failures, 1);
    }

    #[test]
    fn client_missing_baseline_fails_closed_and_network_size_is_checked_first() {
        let base_snapshot = fixture(50);
        let target_snapshot = fixture(51);
        let mut authority = AuthoritySnapshotHistory::new(match_id(), 2).unwrap();
        let base = authority.record_snapshot(&base_snapshot).unwrap();
        authority.record_snapshot(&target_snapshot).unwrap();
        let message = expect_delta(authority.build_latest_delta(base, &[]).unwrap());

        let mut client = ClientBaselineHistory::new(match_id(), 2).unwrap();
        assert_eq!(
            client.apply_delta(&message),
            Err(ClientStateSyncError::BaselineMissing(base))
        );
        assert_eq!(client.len(), 0);
        assert_eq!(client.metrics().baseline_misses, 1);

        let oversized = vec![0; MAX_RESYNC_SNAPSHOT_BYTES + 1];
        assert_eq!(
            authority.record_encoded(StateBaseline::default(), &oversized),
            Err(StateSyncError::SnapshotTooLarge {
                bytes: MAX_RESYNC_SNAPSHOT_BYTES + 1,
                maximum: MAX_RESYNC_SNAPSHOT_BYTES,
            })
        );
    }

    #[test]
    fn peer_coordinator_supports_multiple_seats_and_independent_peer_baselines() {
        let peer_a = peer_id(1);
        let peer_b = peer_id(2);
        let ownership = SeatOwnership::from_assignments(&[
            SeatAssignment {
                seat: SeatId::new(0).unwrap(),
                fighter: FighterId::new(0).unwrap(),
                owner: SeatOwner::Peer(peer_a),
            },
            SeatAssignment {
                seat: SeatId::new(1).unwrap(),
                fighter: FighterId::new(1).unwrap(),
                owner: SeatOwner::Peer(peer_a),
            },
            SeatAssignment {
                seat: SeatId::new(2).unwrap(),
                fighter: FighterId::new(2).unwrap(),
                owner: SeatOwner::Peer(peer_b),
            },
        ])
        .unwrap();
        let ticks = InputTickWindow::new(SimTick(90), SimTick(100), SimTick(100)).unwrap();

        let mut authority = AuthoritySnapshotHistory::new(match_id(), 3).unwrap();
        let base = authority.record_snapshot(&fixture(60)).unwrap();
        let target = authority.record_snapshot(&fixture(61)).unwrap();
        let mut peers = AuthorityStateSyncCoordinator::with_maximum_capacity(match_id()).unwrap();
        assert_eq!(
            peers.connect_peer(peer_a).unwrap(),
            PeerRegistrationOutcome::Connected
        );
        assert_eq!(
            peers.connect_peer(peer_b).unwrap(),
            PeerRegistrationOutcome::Connected
        );

        let batch_a = input_batch(match_id(), peer_a, &[0, 1], Some(base));
        let batch_b = input_batch(match_id(), peer_b, &[2], Some(base));
        batch_a
            .validate_for(match_id(), peer_a, &ownership, &ticks)
            .unwrap();
        batch_b
            .validate_for(match_id(), peer_b, &ownership, &ticks)
            .unwrap();
        assert!(matches!(
            peers
                .observe_validated_input_batch(peer_a, &batch_a, &authority)
                .unwrap(),
            PeerBaselineAckOutcome::Accepted {
                previous: None,
                acknowledged
            } if acknowledged == base
        ));
        assert!(matches!(
            peers
                .observe_validated_input_batch(peer_b, &batch_b, &authority)
                .unwrap(),
            PeerBaselineAckOutcome::Accepted {
                previous: None,
                acknowledged
            } if acknowledged == base
        ));

        for peer_id in [peer_a, peer_b] {
            assert!(matches!(
                peers
                    .build_latest_for_peer(&mut authority, peer_id, &[])
                    .unwrap(),
                PeerStateUpdateOutcome::Delta {
                    peer_id: built_for,
                    message
                } if built_for == peer_id
                    && message.base_tick == base.tick
                    && message.authority_tick == target.tick
                    && message.state_hash == target.hash
            ));
        }
        assert_eq!(peers.len(), 2);
        assert_eq!(peers.metrics().acknowledgements_accepted, 2);
        assert_eq!(peers.metrics().deltas_built, 2);
    }

    #[test]
    fn peer_without_ack_waits_for_initial_full_snapshot_baseline() {
        let peer = peer_id(3);
        let mut authority = AuthoritySnapshotHistory::new(match_id(), 2).unwrap();
        let target = authority.record_snapshot(&fixture(70)).unwrap();
        let mut peers = AuthorityStateSyncCoordinator::new(match_id(), 1).unwrap();
        peers.connect_peer(peer).unwrap();
        let batch = input_batch(match_id(), peer, &[0], None);

        assert_eq!(
            peers
                .observe_validated_input_batch(peer, &batch, &authority)
                .unwrap(),
            PeerBaselineAckOutcome::NoAcknowledgement { retained: None }
        );
        assert_eq!(
            peers
                .build_latest_for_peer(&mut authority, peer, &[])
                .unwrap(),
            PeerStateUpdateOutcome::AwaitingBaselineAcknowledgement {
                peer_id: peer,
                target,
            }
        );
        assert_eq!(peers.metrics().batches_without_acknowledgement, 1);
        assert_eq!(peers.metrics().updates_awaiting_acknowledgement, 1);
        assert_eq!(authority.metrics().deltas_built, 0);
    }

    #[test]
    fn authority_verified_ack_rejects_future_and_retained_hash_poisoning() {
        let malicious = peer_id(32);
        let expired = peer_id(33);
        let mut authority = AuthoritySnapshotHistory::new(match_id(), 2).unwrap();
        let baseline_80 = authority.record_snapshot(&fixture(80)).unwrap();
        let baseline_81 = authority.record_snapshot(&fixture(81)).unwrap();
        let mut peers = AuthorityStateSyncCoordinator::new(match_id(), 2).unwrap();
        peers.connect_peer(malicious).unwrap();
        peers.connect_peer(expired).unwrap();

        let future = StateBaseline::new(baseline_81.tick.next(), StateHash(0xFFFF));
        assert_eq!(
            peers.observe_validated_input_batch(
                malicious,
                &input_batch(match_id(), malicious, &[0], Some(future)),
                &authority,
            ),
            Err(PeerStateSyncError::FutureAcknowledgement {
                peer_id: malicious,
                latest: baseline_81.tick,
                offered: future.tick,
            })
        );
        assert_eq!(peers.acknowledged_baseline(malicious).unwrap(), None);

        let forged = StateBaseline::new(baseline_80.tick, StateHash(baseline_80.hash.0 ^ 0xFFFF));
        assert_eq!(
            peers.observe_validated_input_batch(
                malicious,
                &input_batch(match_id(), malicious, &[0], Some(forged)),
                &authority,
            ),
            Err(PeerStateSyncError::AuthorityHashMismatch {
                peer_id: malicious,
                tick: baseline_80.tick,
                authority: baseline_80.hash,
                offered: forged.hash,
            })
        );
        assert_eq!(peers.acknowledged_baseline(malicious).unwrap(), None);

        assert!(matches!(
            peers
                .observe_validated_input_batch(
                    malicious,
                    &input_batch(match_id(), malicious, &[0], Some(baseline_80)),
                    &authority,
                )
                .unwrap(),
            PeerBaselineAckOutcome::Accepted {
                previous: None,
                acknowledged,
            } if acknowledged == baseline_80
        ));
        peers
            .observe_validated_input_batch(
                expired,
                &input_batch(match_id(), expired, &[1], Some(baseline_80)),
                &authority,
            )
            .unwrap();

        // Once tick 80 expires, no peer-provided hash for an unverifiable tick
        // may move the authority-verified baseline. The retained tick 80 still
        // selects the bounded FullResyncRequired path.
        authority.record_snapshot(&fixture(82)).unwrap();
        for expired_offer in [
            StateBaseline::new(SimTick(79), StateHash(0xBAD0)),
            StateBaseline::new(baseline_80.tick, StateHash(0xBAD1)),
        ] {
            assert_eq!(
                peers
                    .observe_validated_input_batch(
                        expired,
                        &input_batch(match_id(), expired, &[1], Some(expired_offer)),
                        &authority,
                    )
                    .unwrap(),
                PeerBaselineAckOutcome::IgnoredExpired {
                    retained: Some(baseline_80),
                    offered: expired_offer,
                }
            );
            assert_eq!(
                peers.acknowledged_baseline(expired).unwrap(),
                Some(baseline_80)
            );
        }
        assert!(matches!(
            peers
                .build_latest_for_peer(&mut authority, expired, &[])
                .unwrap(),
            PeerStateUpdateOutcome::FullResyncRequired { .. }
        ));
        assert_eq!(peers.metrics().future_acknowledgements, 1);
        assert_eq!(peers.metrics().authority_hash_mismatches, 1);
        assert_eq!(peers.metrics().expired_acknowledgements, 2);
        assert_eq!(peers.metrics().rejected_batches, 2);
    }

    #[test]
    fn validated_resync_applied_immediately_advances_peer_delta_baseline() {
        let peer = peer_id(30);
        let mut authority = AuthoritySnapshotHistory::new(match_id(), 2).unwrap();
        let baseline = authority.record_snapshot(&fixture(75)).unwrap();
        let newer = authority.record_snapshot(&fixture(76)).unwrap();
        let applied = ResyncApplied {
            match_id: match_id(),
            transfer_id: TransferId::new(7).unwrap(),
            peer_id: peer,
            snapshot_tick: baseline.tick,
            snapshot_hash: baseline.hash,
        };
        let mut peers = AuthorityStateSyncCoordinator::new(match_id(), 1).unwrap();
        peers.connect_peer(peer).unwrap();
        peers
            .observe_validated_input_batch(
                peer,
                &input_batch(match_id(), peer, &[0], Some(newer)),
                &authority,
            )
            .unwrap();

        assert_eq!(
            peers
                .observe_validated_resync_applied(peer, &applied)
                .unwrap(),
            PeerBaselineAckOutcome::Accepted {
                previous: Some(newer),
                acknowledged: baseline,
            }
        );
        assert_eq!(peers.acknowledged_baseline(peer).unwrap(), Some(baseline));
        assert_eq!(
            peers
                .observe_validated_input_batch(
                    peer,
                    &input_batch(match_id(), peer, &[0], Some(baseline)),
                    &authority,
                )
                .unwrap(),
            PeerBaselineAckOutcome::Duplicate(baseline)
        );

        let mut spoofed = applied;
        spoofed.peer_id = peer_id(31);
        assert_eq!(
            peers.observe_validated_resync_applied(peer, &spoofed),
            Err(PeerStateSyncError::PeerMismatch {
                connected: peer,
                claimed: spoofed.peer_id,
            })
        );
    }

    #[test]
    fn stale_duplicate_and_wrong_hash_peer_acks_have_deterministic_outcomes() {
        let peer = peer_id(4);
        let mut authority = AuthoritySnapshotHistory::new(match_id(), 4).unwrap();
        let stale = authority.record_snapshot(&fixture(79)).unwrap();
        let initial = authority.record_snapshot(&fixture(80)).unwrap();
        authority.record_snapshot(&fixture(81)).unwrap();
        authority.record_snapshot(&fixture(82)).unwrap();
        let conflict = StateBaseline::new(initial.tick, StateHash(initial.hash.0 ^ 1));
        let mut peers = AuthorityStateSyncCoordinator::new(match_id(), 1).unwrap();
        peers.connect_peer(peer).unwrap();

        assert!(matches!(
            peers
                .observe_validated_input_batch(
                    peer,
                    &input_batch(match_id(), peer, &[0], Some(initial)),
                    &authority,
                )
                .unwrap(),
            PeerBaselineAckOutcome::Accepted { acknowledged, .. }
                if acknowledged == initial
        ));
        assert_eq!(
            peers
                .observe_validated_input_batch(
                    peer,
                    &input_batch(match_id(), peer, &[0], Some(initial)),
                    &authority,
                )
                .unwrap(),
            PeerBaselineAckOutcome::Duplicate(initial)
        );
        assert_eq!(
            peers
                .observe_validated_input_batch(
                    peer,
                    &input_batch(match_id(), peer, &[0], Some(stale)),
                    &authority,
                )
                .unwrap(),
            PeerBaselineAckOutcome::IgnoredStale {
                retained: initial,
                offered: stale,
            }
        );
        assert_eq!(
            peers.observe_validated_input_batch(
                peer,
                &input_batch(match_id(), peer, &[0], Some(conflict)),
                &authority,
            ),
            Err(PeerStateSyncError::AuthorityHashMismatch {
                peer_id: peer,
                tick: initial.tick,
                authority: initial.hash,
                offered: conflict.hash,
            })
        );
        assert_eq!(peers.acknowledged_baseline(peer).unwrap(), Some(initial));
        assert_eq!(peers.metrics().duplicate_acknowledgements, 1);
        assert_eq!(peers.metrics().stale_acknowledgements, 1);
        assert_eq!(peers.metrics().conflicting_acknowledgements, 0);
        assert_eq!(peers.metrics().authority_hash_mismatches, 1);
        assert_eq!(peers.metrics().rejected_batches, 1);
    }

    #[test]
    fn peer_registry_rejects_identity_conflicts_and_enforces_fixed_capacity() {
        let peer_a = peer_id(5);
        let peer_b = peer_id(6);
        let peer_c = peer_id(7);
        let mut authority = AuthoritySnapshotHistory::new(match_id(), 2).unwrap();
        authority.record_snapshot(&fixture(1)).unwrap();
        let mut peers = AuthorityStateSyncCoordinator::new(match_id(), 2).unwrap();
        assert_eq!(
            peers.connect_peer(peer_a).unwrap(),
            PeerRegistrationOutcome::Connected
        );
        assert_eq!(
            peers.connect_peer(peer_a).unwrap(),
            PeerRegistrationOutcome::AlreadyConnected
        );
        peers.connect_peer(peer_b).unwrap();
        assert_eq!(
            peers.connect_peer(peer_c),
            Err(PeerStateSyncError::PeerCapacityExceeded { capacity: 2 })
        );

        let peer_conflict = input_batch(match_id(), peer_b, &[0], None);
        assert_eq!(
            peers.observe_validated_input_batch(peer_a, &peer_conflict, &authority),
            Err(PeerStateSyncError::PeerMismatch {
                connected: peer_a,
                claimed: peer_b,
            })
        );
        let other_match = MatchId::new(*b"state-sync-test2").unwrap();
        let match_conflict = input_batch(other_match, peer_a, &[0], None);
        assert_eq!(
            peers.observe_validated_input_batch(peer_a, &match_conflict, &authority),
            Err(PeerStateSyncError::MatchMismatch)
        );
        assert_eq!(
            peers.observe_validated_input_batch(
                peer_c,
                &input_batch(match_id(), peer_c, &[0], None),
                &authority,
            ),
            Err(PeerStateSyncError::UnknownPeer(peer_c))
        );
        assert_eq!(peers.len(), 2);
        assert_eq!(peers.metrics().duplicate_connects, 1);
        assert_eq!(peers.metrics().rejected_batches, 3);

        assert!(peers.disconnect_peer(peer_a));
        assert!(!peers.disconnect_peer(peer_a));
        assert_eq!(
            peers.connect_peer(peer_c).unwrap(),
            PeerRegistrationOutcome::Connected
        );
        assert_eq!(peers.len(), 2);
    }

    #[test]
    fn history_capacity_is_explicitly_bounded() {
        assert!(matches!(
            AuthoritySnapshotHistory::new(match_id(), 1),
            Err(StateSyncError::InvalidHistoryCapacity { .. })
        ));
        assert!(matches!(
            ClientBaselineHistory::new(match_id(), MAX_STATE_SYNC_HISTORY_ENTRIES + 1),
            Err(StateSyncError::InvalidHistoryCapacity { .. })
        ));
        assert!(StateDeltaAndAcks::MAX_WIRE_BYTES <= 1_200);
        assert_eq!(MAX_STATE_DELTA_BYTES, 960);
        assert_eq!(MAX_STATE_SYNC_PEERS, MAX_SEATS);
        assert!(matches!(
            AuthorityStateSyncCoordinator::new(match_id(), 0),
            Err(PeerStateSyncError::InvalidPeerCapacity { .. })
        ));

        let baseline = StateBaseline::new(SimTick(7), StateHash(9));
        assert_eq!(
            StateBaseline::from(StateBaselineAck::from(baseline)),
            baseline
        );
    }
}
