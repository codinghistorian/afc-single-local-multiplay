//! Confirmed-only result, statistic, achievement, and progression handoff.
//!
//! Predicted simulation events never enter this ledger. A record is admitted only
//! after the reliable authority result identifier agrees with the final canonical
//! snapshot byte-for-byte through its hash. Platform backends consume records by
//! [`ConfirmedResultKey`] and must make that key idempotent in durable storage.

use bevy::prelude::Resource;
use std::collections::VecDeque;
use std::error::Error;
use std::fmt;

use crate::headless::{snapshot_contract_for_manifest, snapshot_gameplay_content_hash};
use crate::network_protocol::{
    AuthorityKind, MatchId, MatchManifest, ProtocolValidationError, SimTick, StateHash,
};
use crate::session::ConfirmedSessionResult;
use crate::snapshot::{
    CanonicalSnapshot, MatchPhaseSnapshot, MatchResultSnapshot, MatchStatsSnapshot, SnapshotError,
};

/// Results retained in one client process. Applied entries may be evicted at this
/// bound; durable sinks still deduplicate forever by [`ConfirmedResultKey`].
pub const CONFIRMED_RESULT_LEDGER_CAPACITY: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ConfirmedResultKey {
    pub match_id: MatchId,
    pub result_id: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgressionTrust {
    /// Offline and listen-authority results can drive local presentation and
    /// explicitly casual progression, but never ranked or trusted rewards.
    UntrustedCasual,
    /// The manifest was validated as a dedicated authority with trusted results.
    TrustedDedicated,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfirmedProgressionRecord {
    pub key: ConfirmedResultKey,
    pub final_tick: SimTick,
    pub final_hash: StateHash,
    pub result: MatchResultSnapshot,
    pub stats: MatchStatsSnapshot,
    pub trust: ProgressionTrust,
}

impl ConfirmedProgressionRecord {
    /// Binds a reliable result identifier to the exact final canonical snapshot.
    /// This is the only constructor exposed to progression consumers.
    pub fn validate_and_build(
        manifest: &MatchManifest,
        confirmed: ConfirmedSessionResult,
        snapshot: &CanonicalSnapshot,
    ) -> Result<Self, ConfirmedProgressionError> {
        manifest.validate()?;
        if confirmed.result_id == 0 {
            return Err(ConfirmedProgressionError::ZeroResultId);
        }
        if manifest.match_id.as_bytes() != &snapshot.header.match_id {
            return Err(ConfirmedProgressionError::MatchMismatch);
        }
        if snapshot.header.tick != confirmed.final_tick {
            return Err(ConfirmedProgressionError::FinalTickMismatch {
                confirmed: confirmed.final_tick,
                snapshot: snapshot.header.tick,
            });
        }

        let contract = snapshot_contract_for_manifest(manifest);
        if snapshot.header.simulation_version != contract.simulation_version
            || snapshot.header.protocol_version != contract.protocol_version
            || snapshot.header.gameplay_content_hash
                != snapshot_gameplay_content_hash(manifest.compatibility.gameplay_content)
            || snapshot.header.master_seed != contract.master_seed
        {
            return Err(ConfirmedProgressionError::SnapshotContractMismatch);
        }

        let actual_hash = StateHash(snapshot.canonical_hash()?);
        if actual_hash != confirmed.final_hash {
            return Err(ConfirmedProgressionError::FinalHashMismatch {
                confirmed: confirmed.final_hash,
                snapshot: actual_hash,
            });
        }
        if !matches!(
            snapshot.match_state.phase,
            MatchPhaseSnapshot::Result | MatchPhaseSnapshot::TimeUp
        ) {
            return Err(ConfirmedProgressionError::SnapshotNotFinal);
        }
        let decided_tick = match snapshot.match_state.result {
            MatchResultSnapshot::Pending => {
                return Err(ConfirmedProgressionError::PendingResult);
            }
            MatchResultSnapshot::Draw { decided_tick }
            | MatchResultSnapshot::FighterWinner { decided_tick, .. }
            | MatchResultSnapshot::TeamWinner { decided_tick, .. }
            | MatchResultSnapshot::Aborted { decided_tick, .. } => decided_tick,
        };
        if decided_tick > confirmed.final_tick {
            return Err(ConfirmedProgressionError::ResultFromFuture {
                decided_tick,
                final_tick: confirmed.final_tick,
            });
        }

        let trust = if manifest.trusted_results {
            if manifest.authority != AuthorityKind::Dedicated {
                // `MatchManifest::validate` already enforces this; retain the
                // explicit check at the security boundary for audit clarity.
                return Err(ConfirmedProgressionError::UntrustedAuthority);
            }
            ProgressionTrust::TrustedDedicated
        } else {
            ProgressionTrust::UntrustedCasual
        };
        Ok(Self {
            key: ConfirmedResultKey {
                match_id: manifest.match_id,
                result_id: confirmed.result_id,
            },
            final_tick: confirmed.final_tick,
            final_hash: confirmed.final_hash,
            result: snapshot.match_state.result,
            stats: snapshot.stats.clone(),
            trust,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfirmedResultObservation {
    Accepted,
    Duplicate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfirmedResultAcknowledgement {
    Applied,
    AlreadyApplied,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConfirmedProgressionMetrics {
    pub accepted: u64,
    pub duplicates: u64,
    pub applied: u64,
    pub evicted_applied: u64,
    pub rejected: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum LedgerState {
    Pending,
    Applied,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LedgerEntry {
    record: ConfirmedProgressionRecord,
    state: LedgerState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PreparedObservationKind {
    Accepted {
        evict_applied: Option<ConfirmedResultKey>,
    },
    Duplicate,
}

/// A fallible confirmed-result observation which has been completely checked
/// against the current ledger but has not mutated it yet.
///
/// The render world is exclusively borrowed between preparation and commit.
/// That lets callers validate the progression transaction, install the exact
/// final snapshot, and then commit this token without any fallible work after
/// the visible result changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedConfirmedProgressionObservation {
    record: ConfirmedProgressionRecord,
    kind: PreparedObservationKind,
    retire_untrusted: bool,
    expected_ledger_len: usize,
}

impl PreparedConfirmedProgressionObservation {
    pub const fn record(&self) -> &ConfirmedProgressionRecord {
        &self.record
    }
}

/// Bounded client-side handoff for a platform progression backend.
///
/// Callers inspect [`Self::next_pending`], submit using `record.key` as the
/// backend idempotency key, and call [`Self::acknowledge`] only after the backend
/// accepts or reports the same key as already applied.
#[derive(Resource, Debug)]
pub struct ConfirmedProgressionLedger {
    entries: VecDeque<LedgerEntry>,
    metrics: ConfirmedProgressionMetrics,
}

impl Default for ConfirmedProgressionLedger {
    fn default() -> Self {
        Self {
            entries: VecDeque::with_capacity(CONFIRMED_RESULT_LEDGER_CAPACITY),
            metrics: ConfirmedProgressionMetrics::default(),
        }
    }
}

impl ConfirmedProgressionLedger {
    pub fn prepare_observation(
        &mut self,
        manifest: &MatchManifest,
        confirmed: ConfirmedSessionResult,
        snapshot: &CanonicalSnapshot,
        retire_untrusted: bool,
    ) -> Result<PreparedConfirmedProgressionObservation, ConfirmedProgressionError> {
        let record =
            match ConfirmedProgressionRecord::validate_and_build(manifest, confirmed, snapshot) {
                Ok(record) => record,
                Err(error) => {
                    self.metrics.rejected = self.metrics.rejected.saturating_add(1);
                    return Err(error);
                }
            };
        if let Some(existing) = self
            .entries
            .iter()
            .find(|entry| entry.record.key.match_id == record.key.match_id)
        {
            if existing.record == record {
                return Ok(PreparedConfirmedProgressionObservation {
                    record,
                    kind: PreparedObservationKind::Duplicate,
                    retire_untrusted,
                    expected_ledger_len: self.entries.len(),
                });
            }
            self.metrics.rejected = self.metrics.rejected.saturating_add(1);
            return Err(ConfirmedProgressionError::ConflictingMatchResult);
        }

        let evict_applied = if self.entries.len() == CONFIRMED_RESULT_LEDGER_CAPACITY {
            let Some(entry) = self
                .entries
                .iter()
                .find(|entry| entry.state == LedgerState::Applied)
            else {
                self.metrics.rejected = self.metrics.rejected.saturating_add(1);
                return Err(ConfirmedProgressionError::LedgerFull);
            };
            Some(entry.record.key)
        } else {
            None
        };
        Ok(PreparedConfirmedProgressionObservation {
            record,
            kind: PreparedObservationKind::Accepted { evict_applied },
            retire_untrusted,
            expected_ledger_len: self.entries.len(),
        })
    }

    /// Commits a previously validated token. This operation is infallible
    /// because the caller holds exclusive access to the render [`World`]
    /// between preparation and commit.
    pub fn commit_prepared(
        &mut self,
        prepared: PreparedConfirmedProgressionObservation,
    ) -> ConfirmedResultObservation {
        assert_eq!(
            self.entries.len(),
            prepared.expected_ledger_len,
            "confirmed progression ledger changed during an atomic projection"
        );
        match prepared.kind {
            PreparedObservationKind::Duplicate => {
                let existing = self
                    .entries
                    .iter_mut()
                    .find(|entry| entry.record.key.match_id == prepared.record.key.match_id)
                    .expect("prepared duplicate remains present");
                assert_eq!(
                    existing.record, prepared.record,
                    "prepared duplicate changed before commit"
                );
                if prepared.retire_untrusted
                    && existing.record.trust == ProgressionTrust::UntrustedCasual
                    && existing.state == LedgerState::Pending
                {
                    existing.state = LedgerState::Applied;
                    self.metrics.applied = self.metrics.applied.saturating_add(1);
                }
                self.metrics.duplicates = self.metrics.duplicates.saturating_add(1);
                ConfirmedResultObservation::Duplicate
            }
            PreparedObservationKind::Accepted { evict_applied } => {
                if let Some(key) = evict_applied {
                    let index = self
                        .entries
                        .iter()
                        .position(|entry| {
                            entry.record.key == key && entry.state == LedgerState::Applied
                        })
                        .expect("prepared applied eviction remains present");
                    self.entries.remove(index);
                    self.metrics.evicted_applied = self.metrics.evicted_applied.saturating_add(1);
                }
                let state = if prepared.retire_untrusted
                    && prepared.record.trust == ProgressionTrust::UntrustedCasual
                {
                    self.metrics.applied = self.metrics.applied.saturating_add(1);
                    LedgerState::Applied
                } else {
                    LedgerState::Pending
                };
                self.entries.push_back(LedgerEntry {
                    record: prepared.record,
                    state,
                });
                self.metrics.accepted = self.metrics.accepted.saturating_add(1);
                ConfirmedResultObservation::Accepted
            }
        }
    }

    pub fn observe(
        &mut self,
        manifest: &MatchManifest,
        confirmed: ConfirmedSessionResult,
        snapshot: &CanonicalSnapshot,
    ) -> Result<ConfirmedResultObservation, ConfirmedProgressionError> {
        let prepared = self.prepare_observation(manifest, confirmed, snapshot, false)?;
        Ok(self.commit_prepared(prepared))
    }

    /// Validates and records a confirmed result using the first-release
    /// progression policy.
    ///
    /// Private/friends listen and offline results are deliberately untrusted:
    /// they can drive the local result presentation, but the first release has
    /// no durable casual reward/statistics sink. Mark those records applied
    /// immediately after validation so the bounded handoff cannot fill after
    /// many matches. A future trusted-dedicated result remains pending until a
    /// durable backend acknowledges its idempotency key.
    pub fn observe_and_retire_untrusted(
        &mut self,
        manifest: &MatchManifest,
        confirmed: ConfirmedSessionResult,
        snapshot: &CanonicalSnapshot,
    ) -> Result<ConfirmedResultObservation, ConfirmedProgressionError> {
        let prepared = self.prepare_observation(manifest, confirmed, snapshot, true)?;
        Ok(self.commit_prepared(prepared))
    }

    pub fn next_pending(&self) -> Option<&ConfirmedProgressionRecord> {
        self.entries
            .iter()
            .find(|entry| entry.state == LedgerState::Pending)
            .map(|entry| &entry.record)
    }

    pub fn pending(&self) -> impl Iterator<Item = &ConfirmedProgressionRecord> {
        self.entries
            .iter()
            .filter(|entry| entry.state == LedgerState::Pending)
            .map(|entry| &entry.record)
    }

    pub fn acknowledge(
        &mut self,
        key: ConfirmedResultKey,
    ) -> Result<ConfirmedResultAcknowledgement, ConfirmedProgressionError> {
        let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.record.key == key)
        else {
            return Err(ConfirmedProgressionError::UnknownResult);
        };
        if entry.state == LedgerState::Applied {
            return Ok(ConfirmedResultAcknowledgement::AlreadyApplied);
        }
        entry.state = LedgerState::Applied;
        self.metrics.applied = self.metrics.applied.saturating_add(1);
        Ok(ConfirmedResultAcknowledgement::Applied)
    }

    pub const fn metrics(&self) -> ConfirmedProgressionMetrics {
        self.metrics
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Debug)]
pub enum ConfirmedProgressionError {
    Protocol(ProtocolValidationError),
    Snapshot(SnapshotError),
    ZeroResultId,
    MatchMismatch,
    FinalTickMismatch {
        confirmed: SimTick,
        snapshot: SimTick,
    },
    SnapshotContractMismatch,
    FinalHashMismatch {
        confirmed: StateHash,
        snapshot: StateHash,
    },
    SnapshotNotFinal,
    PendingResult,
    ResultFromFuture {
        decided_tick: SimTick,
        final_tick: SimTick,
    },
    UntrustedAuthority,
    ConflictingMatchResult,
    LedgerFull,
    UnknownResult,
}

impl From<ProtocolValidationError> for ConfirmedProgressionError {
    fn from(error: ProtocolValidationError) -> Self {
        Self::Protocol(error)
    }
}

impl From<SnapshotError> for ConfirmedProgressionError {
    fn from(error: SnapshotError) -> Self {
        Self::Snapshot(error)
    }
}

impl fmt::Display for ConfirmedProgressionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "confirmed progression rejected: {self:?}")
    }
}

impl Error for ConfirmedProgressionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Protocol(error) => Some(error),
            Self::Snapshot(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::game_state::LocalSetup;
    use crate::headless::build_headless_simulation;
    use crate::match_config::{MatchBuildOptions, build_headless_match_config};
    use crate::network_protocol::{PeerId, SimTick};
    use std::sync::OnceLock;

    fn base_fixture() -> &'static (MatchManifest, CanonicalSnapshot) {
        static BASE: OnceLock<(MatchManifest, CanonicalSnapshot)> = OnceLock::new();
        BASE.get_or_init(|| {
            let setup = LocalSetup::default();
            let peer = PeerId::new(7).unwrap();
            let config = build_headless_match_config(
                &setup,
                MatchBuildOptions::single_peer(
                    MatchId::new([0x44; 16]).unwrap(),
                    AuthorityKind::Offline,
                    false,
                    peer,
                    &setup,
                    SimTick(2),
                ),
            )
            .unwrap();
            let manifest = config.manifest;
            let snapshot = build_headless_simulation(config)
                .unwrap()
                .capture_live_snapshot()
                .unwrap();
            (manifest, snapshot)
        })
    }

    fn fixture(
        serial: u8,
        authority: AuthorityKind,
        trusted_results: bool,
    ) -> (MatchManifest, CanonicalSnapshot, ConfirmedSessionResult) {
        let (base_manifest, base_snapshot) = base_fixture();
        let mut manifest = *base_manifest;
        manifest.authority = authority;
        manifest.trusted_results = trusted_results;
        let mut match_bytes = [serial; 16];
        match_bytes[15] = serial.max(1);
        manifest.match_id = MatchId::new(match_bytes).unwrap();
        let mut snapshot = base_snapshot.clone();
        snapshot.header.match_id = match_bytes;
        let final_tick = SimTick(120 + u64::from(serial));
        snapshot.header.tick = final_tick;
        snapshot.match_state.phase = MatchPhaseSnapshot::Result;
        snapshot.match_state.result = MatchResultSnapshot::Draw {
            decided_tick: final_tick,
        };
        snapshot.stats.gameplay_ticks = final_tick.get();
        let confirmed = ConfirmedSessionResult {
            result_id: u64::from(serial) + 1,
            final_tick,
            final_hash: StateHash(snapshot.canonical_hash().unwrap()),
        };
        (manifest, snapshot, confirmed)
    }

    #[test]
    fn authority_confirmation_is_required_and_duplicate_delivery_is_idempotent() {
        let (manifest, snapshot, confirmed) = fixture(1, AuthorityKind::Offline, false);
        let mut ledger = ConfirmedProgressionLedger::default();
        assert_eq!(
            ledger.observe(&manifest, confirmed, &snapshot).unwrap(),
            ConfirmedResultObservation::Accepted
        );
        assert_eq!(
            ledger.observe(&manifest, confirmed, &snapshot).unwrap(),
            ConfirmedResultObservation::Duplicate
        );
        assert_eq!(ledger.len(), 1);
        let key = ledger.next_pending().unwrap().key;
        assert_eq!(
            ledger.acknowledge(key).unwrap(),
            ConfirmedResultAcknowledgement::Applied
        );
        assert_eq!(
            ledger.acknowledge(key).unwrap(),
            ConfirmedResultAcknowledgement::AlreadyApplied
        );
        assert!(ledger.next_pending().is_none());
    }

    #[test]
    fn prepared_observation_does_not_mutate_until_infallible_commit() {
        let (manifest, snapshot, confirmed) = fixture(11, AuthorityKind::Listen, false);
        let mut ledger = ConfirmedProgressionLedger::default();
        let prepared = ledger
            .prepare_observation(&manifest, confirmed, &snapshot, true)
            .unwrap();
        assert!(ledger.is_empty());
        assert_eq!(ledger.metrics().accepted, 0);

        assert_eq!(
            ledger.commit_prepared(prepared),
            ConfirmedResultObservation::Accepted
        );
        assert_eq!(ledger.len(), 1);
        assert!(ledger.next_pending().is_none());
        assert_eq!(ledger.metrics().accepted, 1);
        assert_eq!(ledger.metrics().applied, 1);
    }

    #[test]
    fn mismatched_hash_tick_match_and_nonfinal_snapshots_fail_closed() {
        let (manifest, snapshot, confirmed) = fixture(2, AuthorityKind::Offline, false);
        let mut ledger = ConfirmedProgressionLedger::default();

        let mut bad = confirmed;
        bad.final_hash.0 ^= 1;
        assert!(matches!(
            ledger.observe(&manifest, bad, &snapshot),
            Err(ConfirmedProgressionError::FinalHashMismatch { .. })
        ));
        bad = confirmed;
        bad.final_tick = bad.final_tick.next();
        assert!(matches!(
            ledger.observe(&manifest, bad, &snapshot),
            Err(ConfirmedProgressionError::FinalTickMismatch { .. })
        ));
        let mut nonfinal = snapshot.clone();
        nonfinal.match_state.phase = MatchPhaseSnapshot::Fight;
        let nonfinal_confirmation = ConfirmedSessionResult {
            final_hash: StateHash(nonfinal.canonical_hash().unwrap()),
            ..confirmed
        };
        assert!(matches!(
            ledger.observe(&manifest, nonfinal_confirmation, &nonfinal),
            Err(ConfirmedProgressionError::SnapshotNotFinal)
        ));
    }

    #[test]
    fn only_validated_dedicated_results_receive_trusted_scope() {
        let (casual_manifest, casual_snapshot, casual) = fixture(3, AuthorityKind::Listen, false);
        let casual_record = ConfirmedProgressionRecord::validate_and_build(
            &casual_manifest,
            casual,
            &casual_snapshot,
        )
        .unwrap();
        assert_eq!(casual_record.trust, ProgressionTrust::UntrustedCasual);

        let (trusted_manifest, trusted_snapshot, trusted) =
            fixture(4, AuthorityKind::Dedicated, true);
        let trusted_record = ConfirmedProgressionRecord::validate_and_build(
            &trusted_manifest,
            trusted,
            &trusted_snapshot,
        )
        .unwrap();
        assert_eq!(trusted_record.trust, ProgressionTrust::TrustedDedicated);
    }

    #[test]
    fn conflicting_result_for_one_match_is_rejected() {
        let (manifest, snapshot, confirmed) = fixture(5, AuthorityKind::Offline, false);
        let mut ledger = ConfirmedProgressionLedger::default();
        ledger.observe(&manifest, confirmed, &snapshot).unwrap();
        let conflict = ConfirmedSessionResult {
            result_id: confirmed.result_id + 1,
            ..confirmed
        };
        assert!(matches!(
            ledger.observe(&manifest, conflict, &snapshot),
            Err(ConfirmedProgressionError::ConflictingMatchResult)
        ));
    }

    #[test]
    fn bounded_ledger_never_evicts_unsubmitted_results() {
        let mut ledger = ConfirmedProgressionLedger::default();
        for serial in 1..=CONFIRMED_RESULT_LEDGER_CAPACITY as u8 {
            let (manifest, snapshot, confirmed) = fixture(serial, AuthorityKind::Offline, false);
            ledger.observe(&manifest, confirmed, &snapshot).unwrap();
        }
        let (manifest, snapshot, confirmed) = fixture(100, AuthorityKind::Offline, false);
        assert!(matches!(
            ledger.observe(&manifest, confirmed, &snapshot),
            Err(ConfirmedProgressionError::LedgerFull)
        ));

        let first = ledger.next_pending().unwrap().key;
        ledger.acknowledge(first).unwrap();
        assert_eq!(
            ledger.observe(&manifest, confirmed, &snapshot).unwrap(),
            ConfirmedResultObservation::Accepted
        );
        assert_eq!(ledger.len(), CONFIRMED_RESULT_LEDGER_CAPACITY);
        assert_eq!(ledger.metrics().evicted_applied, 1);
    }

    #[test]
    fn first_release_retires_untrusted_results_and_never_fills_after_many_matches() {
        let mut ledger = ConfirmedProgressionLedger::default();
        let match_count = CONFIRMED_RESULT_LEDGER_CAPACITY + 16;
        for serial in 1..=match_count as u8 {
            let (manifest, snapshot, confirmed) = fixture(serial, AuthorityKind::Listen, false);
            assert_eq!(
                ledger
                    .observe_and_retire_untrusted(&manifest, confirmed, &snapshot)
                    .unwrap(),
                ConfirmedResultObservation::Accepted
            );
        }

        assert_eq!(ledger.len(), CONFIRMED_RESULT_LEDGER_CAPACITY);
        assert!(ledger.next_pending().is_none());
        assert_eq!(ledger.metrics().accepted, match_count as u64);
        assert_eq!(ledger.metrics().applied, match_count as u64);
        assert_eq!(ledger.metrics().evicted_applied, 16);
        assert_eq!(ledger.metrics().rejected, 0);
    }

    #[test]
    fn trusted_dedicated_result_still_waits_for_a_durable_sink() {
        let (manifest, snapshot, confirmed) = fixture(90, AuthorityKind::Dedicated, true);
        let mut ledger = ConfirmedProgressionLedger::default();

        ledger
            .observe_and_retire_untrusted(&manifest, confirmed, &snapshot)
            .unwrap();

        let pending = ledger
            .next_pending()
            .expect("trusted results require durable acknowledgement");
        assert_eq!(pending.key.match_id, manifest.match_id);
        assert_eq!(pending.trust, ProgressionTrust::TrustedDedicated);
        assert_eq!(ledger.metrics().applied, 0);
    }
}
