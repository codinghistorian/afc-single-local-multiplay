//! Bounded authority-side abuse policy and local ban bridge.
//!
//! Packet parsing and input validation remain in their owning layers. This
//! module turns already-classified violations into stable warn/kick/ban actions
//! without retaining attacker-controlled strings or authentication material.

use core::fmt;

use crate::network_protocol::{
    DisconnectCode, DisconnectMessage, MatchId, PeerId, RetryDisposition,
};
use crate::reconnect::AuthenticatedUserId;
use crate::simulation::SimTick;

pub const MAX_LOCAL_BAN_ENTRIES: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecurityViolation {
    MalformedEnvelope,
    OversizedDatagram,
    DecodeRejected,
    WrongDirection,
    ReceiveBudgetFlood,
    QueueFlood,
    ReliableWindowAbuse,
    ConflictingIdempotentMessage,
    SpoofedIdentity,
    InvalidSeatOwnership,
    InvalidInput,
    InvalidSessionTransition,
    AuthenticationRevoked,
    PlatformBan,
}

impl SecurityViolation {
    pub const fn score(self) -> u16 {
        match self {
            Self::MalformedEnvelope | Self::DecodeRejected => 2,
            Self::OversizedDatagram
            | Self::WrongDirection
            | Self::ReceiveBudgetFlood
            | Self::QueueFlood
            | Self::ReliableWindowAbuse => 4,
            Self::ConflictingIdempotentMessage
            | Self::InvalidInput
            | Self::InvalidSessionTransition => 8,
            Self::SpoofedIdentity | Self::InvalidSeatOwnership => 16,
            Self::AuthenticationRevoked | Self::PlatformBan => u16::MAX,
        }
    }

    pub const fn disconnect_code(self) -> DisconnectCode {
        match self {
            Self::MalformedEnvelope
            | Self::OversizedDatagram
            | Self::DecodeRejected
            | Self::WrongDirection
            | Self::ReliableWindowAbuse
            | Self::ConflictingIdempotentMessage
            | Self::InvalidSessionTransition => DisconnectCode::MalformedTraffic,
            Self::ReceiveBudgetFlood | Self::QueueFlood => DisconnectCode::RateLimited,
            Self::InvalidInput => DisconnectCode::InvalidInput,
            Self::SpoofedIdentity | Self::InvalidSeatOwnership => DisconnectCode::OwnershipFailed,
            Self::AuthenticationRevoked | Self::PlatformBan => DisconnectCode::AuthenticationFailed,
        }
    }

    pub const fn detail_code(self) -> u16 {
        match self {
            Self::MalformedEnvelope => 1,
            Self::OversizedDatagram => 2,
            Self::DecodeRejected => 3,
            Self::WrongDirection => 4,
            Self::ReceiveBudgetFlood => 5,
            Self::QueueFlood => 6,
            Self::ReliableWindowAbuse => 7,
            Self::ConflictingIdempotentMessage => 8,
            Self::SpoofedIdentity => 9,
            Self::InvalidSeatOwnership => 10,
            Self::InvalidInput => 11,
            Self::InvalidSessionTransition => 12,
            Self::AuthenticationRevoked => 13,
            Self::PlatformBan => 14,
        }
    }

    pub const fn forces_ban(self) -> bool {
        matches!(self, Self::PlatformBan)
    }

    pub const fn forces_kick(self) -> bool {
        matches!(self, Self::AuthenticationRevoked | Self::PlatformBan)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SecurityPolicy {
    pub warning_score: u16,
    pub kick_score: u16,
    pub temporary_ban_score: u16,
    pub score_decay_interval_ticks: u16,
    pub score_decay_amount: u16,
    pub temporary_ban_ticks: u64,
}

impl Default for SecurityPolicy {
    fn default() -> Self {
        Self {
            warning_score: 8,
            kick_score: 24,
            temporary_ban_score: 48,
            score_decay_interval_ticks: 60,
            score_decay_amount: 1,
            temporary_ban_ticks: 10 * 60 * 60,
        }
    }
}

impl SecurityPolicy {
    pub const fn validate(self) -> Result<(), SecurityPolicyError> {
        if self.warning_score == 0
            || self.warning_score >= self.kick_score
            || self.kick_score >= self.temporary_ban_score
            || self.score_decay_interval_ticks == 0
            || self.score_decay_amount == 0
            || self.temporary_ban_ticks == 0
        {
            Err(SecurityPolicyError::InvalidPolicy)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecurityPolicyError {
    InvalidPolicy,
    TimelineRegression,
    BanCapacityExhausted,
}

impl fmt::Display for SecurityPolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid multiplayer security operation: {self:?}"
        )
    }
}

impl std::error::Error for SecurityPolicyError {}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SecurityDisposition {
    #[default]
    Accept,
    Warn,
    Kick,
    TemporaryBan,
    PlatformBan,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SecurityDecision {
    pub disposition: SecurityDisposition,
    pub violation: SecurityViolation,
    pub accumulated_score: u16,
    pub disconnect: Option<DisconnectMessage>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PeerSecurityMetrics {
    pub violations: u64,
    pub warnings: u64,
    pub kicks: u64,
    pub temporary_bans: u64,
    pub platform_bans: u64,
    pub peak_score: u16,
}

/// One allocation-free scorecard owned by one authenticated authority link.
pub struct PeerSecurityGuard {
    policy: SecurityPolicy,
    score: u16,
    last_tick: SimTick,
    decay_remainder: u16,
    metrics: PeerSecurityMetrics,
}

impl PeerSecurityGuard {
    pub fn new(policy: SecurityPolicy, now: SimTick) -> Result<Self, SecurityPolicyError> {
        policy.validate()?;
        Ok(Self {
            policy,
            score: 0,
            last_tick: now,
            decay_remainder: 0,
            metrics: PeerSecurityMetrics::default(),
        })
    }

    pub const fn score(&self) -> u16 {
        self.score
    }

    pub const fn metrics(&self) -> PeerSecurityMetrics {
        self.metrics
    }

    pub fn observe_clean_tick(&mut self, now: SimTick) -> Result<(), SecurityPolicyError> {
        self.advance(now)
    }

    pub fn observe_violation(
        &mut self,
        match_id: Option<MatchId>,
        last_confirmed_tick: Option<SimTick>,
        violation: SecurityViolation,
        now: SimTick,
    ) -> Result<SecurityDecision, SecurityPolicyError> {
        self.advance(now)?;
        self.score = self.score.saturating_add(violation.score());
        self.metrics.violations = self.metrics.violations.saturating_add(1);
        self.metrics.peak_score = self.metrics.peak_score.max(self.score);

        let disposition = if violation == SecurityViolation::PlatformBan {
            SecurityDisposition::PlatformBan
        } else if violation.forces_ban() || self.score >= self.policy.temporary_ban_score {
            SecurityDisposition::TemporaryBan
        } else if violation.forces_kick() || self.score >= self.policy.kick_score {
            SecurityDisposition::Kick
        } else if self.score >= self.policy.warning_score {
            SecurityDisposition::Warn
        } else {
            SecurityDisposition::Accept
        };
        match disposition {
            SecurityDisposition::Accept => {}
            SecurityDisposition::Warn => {
                self.metrics.warnings = self.metrics.warnings.saturating_add(1);
            }
            SecurityDisposition::Kick => {
                self.metrics.kicks = self.metrics.kicks.saturating_add(1);
            }
            SecurityDisposition::TemporaryBan => {
                self.metrics.temporary_bans = self.metrics.temporary_bans.saturating_add(1);
            }
            SecurityDisposition::PlatformBan => {
                self.metrics.platform_bans = self.metrics.platform_bans.saturating_add(1);
            }
        }

        let disconnect = matches!(
            disposition,
            SecurityDisposition::Kick
                | SecurityDisposition::TemporaryBan
                | SecurityDisposition::PlatformBan
        )
        .then_some(DisconnectMessage {
            match_id,
            code: violation.disconnect_code(),
            retry: if disposition == SecurityDisposition::Kick {
                RetryDisposition::ReturnToLobby
            } else {
                RetryDisposition::Fatal
            },
            detail_code: violation.detail_code(),
            last_confirmed_tick,
        });

        Ok(SecurityDecision {
            disposition,
            violation,
            accumulated_score: self.score,
            disconnect,
        })
    }

    fn advance(&mut self, now: SimTick) -> Result<(), SecurityPolicyError> {
        if now < self.last_tick {
            return Err(SecurityPolicyError::TimelineRegression);
        }
        let elapsed = now.0 - self.last_tick.0;
        let combined = elapsed.saturating_add(u64::from(self.decay_remainder));
        let interval = u64::from(self.policy.score_decay_interval_ticks);
        let intervals = combined / interval;
        self.decay_remainder = (combined % interval) as u16;
        let decay = intervals
            .saturating_mul(u64::from(self.policy.score_decay_amount))
            .min(u64::from(u16::MAX)) as u16;
        self.score = self.score.saturating_sub(decay);
        self.last_tick = now;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BanReason {
    RepeatedProtocolAbuse,
    SpoofedIdentity,
    PlatformBan,
    Operator,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BanEntry {
    pub user: AuthenticatedUserId,
    pub reason: BanReason,
    pub issued_at: SimTick,
    /// `None` is an operator/platform permanent ban.
    pub expires_at: Option<SimTick>,
    pub offenses: u16,
}

impl BanEntry {
    pub const fn active_at(self, now: SimTick) -> bool {
        match self.expires_at {
            Some(expires) => now.0 < expires.0,
            None => true,
        }
    }
}

pub trait BanProvider {
    fn lookup(&mut self, user: AuthenticatedUserId, now: SimTick) -> Option<BanEntry>;
    fn record(&mut self, entry: BanEntry) -> Result<(), SecurityPolicyError>;
}

/// Fixed-capacity process-local bridge. Shipping services may implement
/// [`BanProvider`] against their own operator/publisher backend.
pub struct LocalBanRegistry {
    entries: Box<[Option<BanEntry>; MAX_LOCAL_BAN_ENTRIES]>,
    len: usize,
}

impl Default for LocalBanRegistry {
    fn default() -> Self {
        Self {
            entries: Box::new([None; MAX_LOCAL_BAN_ENTRIES]),
            len: 0,
        }
    }
}

impl LocalBanRegistry {
    pub const fn len(&self) -> usize {
        self.len
    }

    pub fn record_temporary(
        &mut self,
        user: AuthenticatedUserId,
        reason: BanReason,
        now: SimTick,
        duration_ticks: u64,
    ) -> Result<BanEntry, SecurityPolicyError> {
        let expires_at = now
            .0
            .checked_add(duration_ticks)
            .map(SimTick)
            .ok_or(SecurityPolicyError::TimelineRegression)?;
        let prior_offenses = self
            .entries
            .iter()
            .flatten()
            .find(|entry| entry.user == user)
            .map_or(0, |entry| entry.offenses);
        let entry = BanEntry {
            user,
            reason,
            issued_at: now,
            expires_at: Some(expires_at),
            offenses: prior_offenses.saturating_add(1),
        };
        self.record(entry)?;
        Ok(entry)
    }

    pub fn record_permanent(
        &mut self,
        user: AuthenticatedUserId,
        reason: BanReason,
        now: SimTick,
    ) -> Result<BanEntry, SecurityPolicyError> {
        let prior_offenses = self
            .entries
            .iter()
            .flatten()
            .find(|entry| entry.user == user)
            .map_or(0, |entry| entry.offenses);
        let entry = BanEntry {
            user,
            reason,
            issued_at: now,
            expires_at: None,
            offenses: prior_offenses.saturating_add(1),
        };
        self.record(entry)?;
        Ok(entry)
    }

    pub fn remove(&mut self, user: AuthenticatedUserId) -> bool {
        let Some(slot) = self
            .entries
            .iter_mut()
            .find(|entry| entry.is_some_and(|entry| entry.user == user))
        else {
            return false;
        };
        *slot = None;
        self.len = self.len.saturating_sub(1);
        true
    }

    pub fn purge_expired(&mut self, now: SimTick) -> usize {
        let mut purged = 0;
        for slot in self.entries.iter_mut() {
            if slot.is_some_and(|entry| !entry.active_at(now)) {
                *slot = None;
                purged += 1;
            }
        }
        self.len = self.len.saturating_sub(purged);
        purged
    }
}

impl BanProvider for LocalBanRegistry {
    fn lookup(&mut self, user: AuthenticatedUserId, now: SimTick) -> Option<BanEntry> {
        let _ = self.purge_expired(now);
        self.entries
            .iter()
            .flatten()
            .copied()
            .find(|entry| entry.user == user && entry.active_at(now))
    }

    fn record(&mut self, entry: BanEntry) -> Result<(), SecurityPolicyError> {
        if let Some(retained) = self
            .entries
            .iter_mut()
            .find(|retained| retained.is_some_and(|retained| retained.user == entry.user))
        {
            *retained = Some(entry);
            return Ok(());
        }
        let Some(slot) = self.entries.iter_mut().find(|slot| slot.is_none()) else {
            return Err(SecurityPolicyError::BanCapacityExhausted);
        };
        *slot = Some(entry);
        self.len += 1;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SecurityAuditEvent {
    pub match_id: Option<MatchId>,
    pub peer_id: Option<PeerId>,
    pub user_id: AuthenticatedUserId,
    pub tick: SimTick,
    pub violation: SecurityViolation,
    pub disposition: SecurityDisposition,
    pub accumulated_score: u16,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(value: u64) -> AuthenticatedUserId {
        AuthenticatedUserId::new(value).unwrap()
    }

    #[test]
    fn score_warns_kicks_and_decays_on_canonical_time() {
        let mut guard = PeerSecurityGuard::new(SecurityPolicy::default(), SimTick::ZERO).unwrap();
        let first = guard
            .observe_violation(None, None, SecurityViolation::WrongDirection, SimTick(1))
            .unwrap();
        assert_eq!(first.disposition, SecurityDisposition::Accept);
        let warning = guard
            .observe_violation(None, None, SecurityViolation::QueueFlood, SimTick(2))
            .unwrap();
        assert_eq!(warning.disposition, SecurityDisposition::Warn);
        let kick = guard
            .observe_violation(None, None, SecurityViolation::SpoofedIdentity, SimTick(3))
            .unwrap();
        assert_eq!(kick.disposition, SecurityDisposition::Kick);
        assert_eq!(
            kick.disconnect.unwrap().code,
            DisconnectCode::OwnershipFailed
        );

        guard.observe_clean_tick(SimTick(3 + 24 * 60)).unwrap();
        assert_eq!(guard.score(), 0);
        assert_eq!(
            guard.observe_clean_tick(SimTick(1)),
            Err(SecurityPolicyError::TimelineRegression)
        );
    }

    #[test]
    fn authentication_revocation_is_immediate_and_platform_ban_is_fatal() {
        let mut guard = PeerSecurityGuard::new(SecurityPolicy::default(), SimTick::ZERO).unwrap();
        let revoked = guard
            .observe_violation(
                None,
                Some(SimTick(9)),
                SecurityViolation::AuthenticationRevoked,
                SimTick(10),
            )
            .unwrap();
        assert_eq!(revoked.disposition, SecurityDisposition::TemporaryBan);
        assert_eq!(
            revoked.disconnect.unwrap().code,
            DisconnectCode::AuthenticationFailed
        );

        let banned = guard
            .observe_violation(None, None, SecurityViolation::PlatformBan, SimTick(11))
            .unwrap();
        assert_eq!(banned.disposition, SecurityDisposition::PlatformBan);
        assert_eq!(banned.disconnect.unwrap().retry, RetryDisposition::Fatal);
    }

    #[test]
    fn local_registry_is_bounded_expires_and_preserves_offense_count() {
        let mut registry = LocalBanRegistry::default();
        let first = registry
            .record_temporary(user(1), BanReason::RepeatedProtocolAbuse, SimTick(10), 5)
            .unwrap();
        assert_eq!(first.offenses, 1);
        assert!(registry.lookup(user(1), SimTick(14)).is_some());
        assert!(registry.lookup(user(1), SimTick(15)).is_none());

        let second = registry
            .record_permanent(user(1), BanReason::Operator, SimTick(16))
            .unwrap();
        // Expiry purges the old entry, so a later operator ban starts a new
        // retained offense record rather than relying on unbounded history.
        assert_eq!(second.offenses, 1);
        assert!(registry.lookup(user(1), SimTick(u64::MAX)).is_some());
        assert!(registry.remove(user(1)));
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn local_registry_fails_closed_at_its_fixed_capacity() {
        let mut registry = LocalBanRegistry::default();
        for value in 1..=MAX_LOCAL_BAN_ENTRIES as u64 {
            registry
                .record_permanent(user(value), BanReason::Operator, SimTick(value))
                .unwrap();
        }
        assert_eq!(registry.len(), MAX_LOCAL_BAN_ENTRIES);
        assert_eq!(
            registry.record_permanent(
                user(MAX_LOCAL_BAN_ENTRIES as u64 + 1),
                BanReason::Operator,
                SimTick(999),
            ),
            Err(SecurityPolicyError::BanCapacityExhausted)
        );
        // Updating an existing record remains possible at capacity.
        assert!(
            registry
                .record_permanent(user(1), BanReason::PlatformBan, SimTick(1_000))
                .is_ok()
        );
    }

    #[test]
    fn invalid_policy_is_rejected() {
        assert_eq!(
            SecurityPolicy {
                kick_score: 8,
                ..SecurityPolicy::default()
            }
            .validate(),
            Err(SecurityPolicyError::InvalidPolicy)
        );
    }
}
