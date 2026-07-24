//! Privacy-safe, bounded multiplayer diagnostics and authority timing metrics.
//!
//! This module deliberately stores numeric identities and stable reason codes
//! only. Authentication tickets, relay credentials, IP addresses, free-form
//! remote text, and platform persona names cannot enter these record types.

use core::fmt;

use crate::determinism::FighterId;
use crate::network_protocol::{MatchId, PeerId, SeatId, SimTick};

pub const ONLINE_AUDIT_CAPACITY: usize = 256;
pub const SERVER_TICK_WINDOW_SAMPLES: usize = 1_024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OnlineAuditScope {
    pub match_id: Option<MatchId>,
    pub peer_id: Option<PeerId>,
    pub seat_id: Option<SeatId>,
    pub fighter_id: Option<FighterId>,
    pub tick: Option<SimTick>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum OnlineAuditCode {
    LobbyEntered = 1,
    LobbyLeft = 2,
    PeerAuthenticated = 3,
    PeerAuthenticationRejected = 4,
    TransportConnected = 5,
    TransportClosed = 6,
    ManifestAccepted = 7,
    InitialSyncApplied = 8,
    CountdownStarted = 9,
    MatchStarted = 10,
    InputRejected = 11,
    InputSubstituted = 12,
    StateMismatch = 13,
    RollbackApplied = 14,
    HardResyncStarted = 15,
    HardResyncApplied = 16,
    PeerDisconnected = 17,
    PeerReconnected = 18,
    PeerKicked = 19,
    PeerTemporarilyBanned = 20,
    AuthorityLost = 21,
    ResultConfirmed = 22,
    QueueHighWater = 23,
    PoolHighWater = 24,
    QualityChanged = 25,
    FatalFailure = 26,
    PeerWarned = 27,
    PeerAdmissionBanned = 28,
    ReconnectGraceExpired = 29,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OnlineAuditRecord {
    pub sequence: u64,
    pub monotonic_ms: u64,
    pub scope: OnlineAuditScope,
    pub code: OnlineAuditCode,
    /// Code-specific bounded numeric detail. Never encode secrets or raw text.
    pub value_a: u64,
    /// Code-specific bounded numeric detail. Never encode secrets or raw text.
    pub value_b: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OnlineAuditMetrics {
    pub retained: u16,
    pub written: u64,
    pub overwritten: u64,
}

/// Oldest-to-newest iterator over the retained fixed ring.
pub struct OnlineAuditIter<'a> {
    log: &'a OnlineAuditLog,
    offset: usize,
}

impl Iterator for OnlineAuditIter<'_> {
    type Item = OnlineAuditRecord;

    fn next(&mut self) -> Option<Self::Item> {
        if self.offset >= self.log.len {
            return None;
        }
        let index = (self.log.start + self.offset) % ONLINE_AUDIT_CAPACITY;
        self.offset += 1;
        self.log.records[index]
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.log.len.saturating_sub(self.offset);
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for OnlineAuditIter<'_> {}

pub struct OnlineAuditLog {
    records: [Option<OnlineAuditRecord>; ONLINE_AUDIT_CAPACITY],
    start: usize,
    len: usize,
    next_sequence: u64,
    last_monotonic_ms: Option<u64>,
    written: u64,
    overwritten: u64,
}

impl Default for OnlineAuditLog {
    fn default() -> Self {
        Self {
            records: [None; ONLINE_AUDIT_CAPACITY],
            start: 0,
            len: 0,
            next_sequence: 1,
            last_monotonic_ms: None,
            written: 0,
            overwritten: 0,
        }
    }
}

impl OnlineAuditLog {
    pub fn push(
        &mut self,
        monotonic_ms: u64,
        scope: OnlineAuditScope,
        code: OnlineAuditCode,
        value_a: u64,
        value_b: u64,
    ) -> Result<OnlineAuditRecord, ObservabilityError> {
        if self
            .last_monotonic_ms
            .is_some_and(|last| monotonic_ms < last)
        {
            return Err(ObservabilityError::ClockRegression);
        }
        let sequence = self.next_sequence;
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or(ObservabilityError::SequenceExhausted)?;
        let record = OnlineAuditRecord {
            sequence,
            monotonic_ms,
            scope,
            code,
            value_a,
            value_b,
        };
        let index = if self.len < ONLINE_AUDIT_CAPACITY {
            let index = (self.start + self.len) % ONLINE_AUDIT_CAPACITY;
            self.len += 1;
            index
        } else {
            let index = self.start;
            self.start = (self.start + 1) % ONLINE_AUDIT_CAPACITY;
            self.overwritten = self.overwritten.saturating_add(1);
            index
        };
        self.records[index] = Some(record);
        self.last_monotonic_ms = Some(monotonic_ms);
        self.written = self.written.saturating_add(1);
        Ok(record)
    }

    pub fn iter(&self) -> OnlineAuditIter<'_> {
        OnlineAuditIter {
            log: self,
            offset: 0,
        }
    }

    pub const fn metrics(&self) -> OnlineAuditMetrics {
        OnlineAuditMetrics {
            retained: self.len as u16,
            written: self.written,
            overwritten: self.overwritten,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ServerTickDistribution {
    pub samples: u16,
    pub p50_ns: u64,
    pub p95_ns: u64,
    pub p99_ns: u64,
    pub maximum_ns: u64,
    pub over_budget: u64,
}

/// Allocation-free rolling authority-step distribution.
pub struct ServerTickWindow {
    samples: [u64; SERVER_TICK_WINDOW_SAMPLES],
    cursor: usize,
    len: usize,
    budget_ns: u64,
    over_budget: u64,
}

impl ServerTickWindow {
    pub fn new(budget_ns: u64) -> Result<Self, ObservabilityError> {
        if budget_ns == 0 {
            return Err(ObservabilityError::InvalidTickBudget);
        }
        Ok(Self {
            samples: [0; SERVER_TICK_WINDOW_SAMPLES],
            cursor: 0,
            len: 0,
            budget_ns,
            over_budget: 0,
        })
    }

    pub fn observe(&mut self, duration_ns: u64) {
        self.samples[self.cursor] = duration_ns;
        self.cursor = (self.cursor + 1) % SERVER_TICK_WINDOW_SAMPLES;
        self.len = (self.len + 1).min(SERVER_TICK_WINDOW_SAMPLES);
        if duration_ns > self.budget_ns {
            self.over_budget = self.over_budget.saturating_add(1);
        }
    }

    pub fn distribution(&self) -> ServerTickDistribution {
        if self.len == 0 {
            return ServerTickDistribution::default();
        }
        let mut ordered = [0_u64; SERVER_TICK_WINDOW_SAMPLES];
        ordered[..self.len].copy_from_slice(&self.samples[..self.len]);
        ordered[..self.len].sort_unstable();
        ServerTickDistribution {
            samples: self.len as u16,
            p50_ns: percentile(&ordered[..self.len], 50),
            p95_ns: percentile(&ordered[..self.len], 95),
            p99_ns: percentile(&ordered[..self.len], 99),
            maximum_ns: ordered[self.len - 1],
            over_budget: self.over_budget,
        }
    }
}

fn percentile(sorted: &[u64], percentile: usize) -> u64 {
    let rank = sorted
        .len()
        .saturating_mul(percentile)
        .div_ceil(100)
        .saturating_sub(1)
        .min(sorted.len() - 1);
    sorted[rank]
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MultiplayerCounterSnapshot {
    pub packets_in: u64,
    pub packets_out: u64,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub inputs_accepted: u64,
    pub inputs_rejected: u64,
    pub inputs_substituted: u64,
    pub rollbacks: u64,
    pub maximum_rollback_depth: u16,
    pub hard_resyncs: u64,
    pub reconnects: u64,
    pub confirmed_hash_mismatches: u64,
    pub queue_high_water: u32,
    pub history_high_water: u32,
    pub pool_high_water: u32,
}

/// Central non-canonical diagnostics state for a client or authority process.
pub struct MultiplayerObservability {
    counters: MultiplayerCounterSnapshot,
    server_ticks: ServerTickWindow,
    audit: OnlineAuditLog,
}

impl MultiplayerObservability {
    pub fn new(server_tick_budget_ns: u64) -> Result<Self, ObservabilityError> {
        Ok(Self {
            counters: MultiplayerCounterSnapshot::default(),
            server_ticks: ServerTickWindow::new(server_tick_budget_ns)?,
            audit: OnlineAuditLog::default(),
        })
    }

    pub const fn counters(&self) -> MultiplayerCounterSnapshot {
        self.counters
    }

    pub fn counters_mut(&mut self) -> &mut MultiplayerCounterSnapshot {
        &mut self.counters
    }

    pub fn audit(&self) -> &OnlineAuditLog {
        &self.audit
    }

    pub fn audit_mut(&mut self) -> &mut OnlineAuditLog {
        &mut self.audit
    }

    pub fn observe_server_tick(&mut self, duration_ns: u64) {
        self.server_ticks.observe(duration_ns);
    }

    pub fn server_tick_distribution(&self) -> ServerTickDistribution {
        self.server_ticks.distribution()
    }

    pub fn observe_rollback_depth(&mut self, depth: u16) {
        self.counters.rollbacks = self.counters.rollbacks.saturating_add(1);
        self.counters.maximum_rollback_depth = self.counters.maximum_rollback_depth.max(depth);
    }

    pub fn observe_queue_depth(&mut self, depth: usize) {
        self.counters.queue_high_water = self
            .counters
            .queue_high_water
            .max(u32::try_from(depth).unwrap_or(u32::MAX));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObservabilityError {
    InvalidTickBudget,
    ClockRegression,
    SequenceExhausted,
}

impl fmt::Display for ObservabilityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid multiplayer observability update: {self:?}"
        )
    }
}

impl std::error::Error for ObservabilityError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_ring_is_bounded_and_iterates_oldest_to_newest() {
        let mut log = OnlineAuditLog::default();
        for index in 0..ONLINE_AUDIT_CAPACITY + 7 {
            log.push(
                index as u64,
                OnlineAuditScope::default(),
                OnlineAuditCode::QueueHighWater,
                index as u64,
                0,
            )
            .unwrap();
        }
        let records: Vec<_> = log.iter().collect();
        assert_eq!(records.len(), ONLINE_AUDIT_CAPACITY);
        assert_eq!(records.first().unwrap().value_a, 7);
        assert_eq!(records.last().unwrap().value_a, 262);
        assert_eq!(
            log.metrics(),
            OnlineAuditMetrics {
                retained: ONLINE_AUDIT_CAPACITY as u16,
                written: 263,
                overwritten: 7,
            }
        );
    }

    #[test]
    fn audit_rejects_regressing_clock_without_mutation() {
        let mut log = OnlineAuditLog::default();
        log.push(
            10,
            OnlineAuditScope::default(),
            OnlineAuditCode::LobbyEntered,
            0,
            0,
        )
        .unwrap();
        assert_eq!(
            log.push(
                9,
                OnlineAuditScope::default(),
                OnlineAuditCode::LobbyLeft,
                0,
                0,
            ),
            Err(ObservabilityError::ClockRegression)
        );
        assert_eq!(log.metrics().written, 1);
    }

    #[test]
    fn tick_distribution_is_exact_and_rolling() {
        let mut window = ServerTickWindow::new(1_000).unwrap();
        for value in 1..=100 {
            window.observe(value * 10);
        }
        assert_eq!(
            window.distribution(),
            ServerTickDistribution {
                samples: 100,
                p50_ns: 500,
                p95_ns: 950,
                p99_ns: 990,
                maximum_ns: 1_000,
                over_budget: 0,
            }
        );
        for _ in 0..SERVER_TICK_WINDOW_SAMPLES {
            window.observe(2_000);
        }
        let rolled = window.distribution();
        assert_eq!(rolled.samples as usize, SERVER_TICK_WINDOW_SAMPLES);
        assert_eq!(rolled.p50_ns, 2_000);
        assert_eq!(rolled.maximum_ns, 2_000);
        assert_eq!(rolled.over_budget, SERVER_TICK_WINDOW_SAMPLES as u64);
    }

    #[test]
    fn counters_saturate_and_track_high_water() {
        let mut observability = MultiplayerObservability::new(1_000_000).unwrap();
        observability.counters_mut().rollbacks = u64::MAX;
        observability.observe_rollback_depth(9);
        observability.observe_queue_depth(7);
        observability.observe_queue_depth(3);
        assert_eq!(observability.counters().rollbacks, u64::MAX);
        assert_eq!(observability.counters().maximum_rollback_depth, 9);
        assert_eq!(observability.counters().queue_high_water, 7);
    }
}
