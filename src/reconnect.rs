//! Transport-independent reconnect ownership and grace-window policy.
//!
//! Steam authentication decides which [`AuthenticatedUserId`] is present. This
//! module decides whether that already-authenticated identity may reclaim the
//! exact peer and seats retained by an active authority. It owns no socket,
//! lobby, Steam callback, or gameplay state.

use core::fmt;

use crate::network_protocol::{
    MAX_SEATS, MatchId, PeerId, ProtocolValidationError, ReconnectClaim, SeatOwner, SeatOwnership,
    SimTick,
};

pub const CASUAL_RECONNECT_GRACE_TICKS: u32 = 15 * 60;
pub const CASUAL_NEUTRAL_INPUT_TICKS: u32 = 2 * 60;

/// Stable platform identity after the platform adapter has authenticated it.
///
/// Steam adapters use the user's 64-bit Steam ID. Tests and non-Steam
/// transports may assign another non-zero stable identity in the same domain.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct AuthenticatedUserId(u64);

impl AuthenticatedUserId {
    pub const fn new(value: u64) -> Option<Self> {
        if value == 0 { None } else { Some(Self(value)) }
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReconnectPolicy {
    pub grace_ticks: u32,
    pub neutral_input_ticks: u32,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            grace_ticks: CASUAL_RECONNECT_GRACE_TICKS,
            neutral_input_ticks: CASUAL_NEUTRAL_INPUT_TICKS,
        }
    }
}

impl ReconnectPolicy {
    pub const fn validate(self) -> Result<(), ReconnectError> {
        if self.grace_ticks == 0 || self.neutral_input_ticks > self.grace_ticks {
            Err(ReconnectError::InvalidPolicy)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthenticatedPeer {
    pub peer_id: PeerId,
    pub user_id: AuthenticatedUserId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubstituteControl {
    Connected,
    NeutralInput,
    BotTakeover,
    /// The reclaim window has closed and these seats remain under deterministic
    /// authority-bot control for the rest of this match.
    PermanentBotReplacement,
}

/// One-shot transition emitted when a disconnected peer's retained seats become
/// permanent authority-bot seats for the remainder of the match.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PermanentBotReplacement {
    pub peer_id: PeerId,
    /// Bit `n` corresponds to protocol seat `n`.
    pub seat_mask: u8,
    /// Exact policy boundary, even if the transition is first observed later.
    pub effective_tick: SimTick,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubstituteControlResolution {
    pub control: SubstituteControl,
    /// Present exactly once for each disconnected peer whose grace expires.
    pub permanent_bot_replacement: Option<PermanentBotReplacement>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReconnectAttemptId(u32);

impl ReconnectAttemptId {
    pub const fn get(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReclaimReservation {
    pub attempt_id: ReconnectAttemptId,
    pub peer_id: PeerId,
    /// Bit `n` corresponds to protocol seat `n`.
    pub seat_mask: u8,
    /// Snapshot and recent canonical inputs are synchronized through this tick.
    pub snapshot_tick: SimTick,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReclaimAdmission {
    pub attempt_id: ReconnectAttemptId,
    pub peer_id: PeerId,
    /// Bit `n` corresponds to protocol seat `n`.
    pub seat_mask: u8,
    /// Exact snapshot acknowledged by the reconnecting client.
    pub snapshot_tick: SimTick,
    /// Reclaimed input may begin only at this tick boundary.
    pub resume_input_tick: SimTick,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReconnectError {
    Protocol(ProtocolValidationError),
    InvalidPolicy,
    EmptyRoster,
    CapacityExceeded,
    DuplicatePeer,
    DuplicateIdentity,
    MissingIdentity(PeerId),
    UnexpectedIdentity(PeerId),
    UnknownPeer(PeerId),
    MatchMismatch,
    IdentityMismatch,
    AlreadyDisconnected,
    NotDisconnected,
    ReclaimInProgress,
    NoReclaimInProgress,
    ReclaimAttemptMismatch,
    SnapshotMismatch,
    GraceExpired,
    ConfirmedTickAhead,
    TimelineRegression,
    TimelineExhausted,
}

impl fmt::Display for ReconnectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "reconnect admission failed: {self:?}")
    }
}

impl std::error::Error for ReconnectError {}

impl From<ProtocolValidationError> for ReconnectError {
    fn from(error: ProtocolValidationError) -> Self {
        Self::Protocol(error)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PeerLease {
    peer_id: PeerId,
    user_id: AuthenticatedUserId,
    seat_mask: u8,
    disconnected_at: Option<SimTick>,
    permanent_bot_since: Option<SimTick>,
    pending_reclaim: Option<PendingReclaim>,
    next_attempt_id: u32,
    last_transition_tick: SimTick,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingReclaim {
    attempt_id: ReconnectAttemptId,
    snapshot_tick: SimTick,
    began_at: SimTick,
}

/// Fixed-capacity reconnect lease table owned by one authority match.
#[derive(Clone, Copy, Debug)]
pub struct ReconnectRegistry {
    match_id: MatchId,
    policy: ReconnectPolicy,
    leases: [Option<PeerLease>; MAX_SEATS],
    lease_count: u8,
}

impl ReconnectRegistry {
    pub fn new(
        match_id: MatchId,
        ownership: &SeatOwnership,
        authenticated_peers: &[AuthenticatedPeer],
        policy: ReconnectPolicy,
    ) -> Result<Self, ReconnectError> {
        match_id.validate()?;
        ownership.validate()?;
        policy.validate()?;
        if authenticated_peers.is_empty() {
            return Err(ReconnectError::EmptyRoster);
        }
        if authenticated_peers.len() > MAX_SEATS {
            return Err(ReconnectError::CapacityExceeded);
        }

        for (index, peer) in authenticated_peers.iter().enumerate() {
            peer.peer_id.validate()?;
            if authenticated_peers[..index]
                .iter()
                .any(|prior| prior.peer_id == peer.peer_id)
            {
                return Err(ReconnectError::DuplicatePeer);
            }
            if authenticated_peers[..index]
                .iter()
                .any(|prior| prior.user_id == peer.user_id)
            {
                return Err(ReconnectError::DuplicateIdentity);
            }
            if !ownership.peer_owns_any_seat(peer.peer_id) {
                return Err(ReconnectError::UnexpectedIdentity(peer.peer_id));
            }
        }

        let mut expected = [None; MAX_SEATS];
        let mut expected_len = 0_usize;
        for assignment in ownership.as_slice() {
            let SeatOwner::Peer(peer_id) = assignment.owner else {
                continue;
            };
            if expected[..expected_len].contains(&Some(peer_id)) {
                continue;
            }
            expected[expected_len] = Some(peer_id);
            expected_len += 1;
        }
        if expected_len == 0 {
            return Err(ReconnectError::EmptyRoster);
        }
        for peer_id in expected[..expected_len].iter().flatten() {
            if !authenticated_peers
                .iter()
                .any(|peer| peer.peer_id == *peer_id)
            {
                return Err(ReconnectError::MissingIdentity(*peer_id));
            }
        }

        let mut registry = Self {
            match_id,
            policy,
            leases: [None; MAX_SEATS],
            lease_count: 0,
        };
        for peer in authenticated_peers {
            let mut seat_mask = 0_u8;
            for assignment in ownership.as_slice() {
                if assignment.owner == SeatOwner::Peer(peer.peer_id) {
                    seat_mask |= 1_u8 << assignment.seat.get();
                }
            }
            registry.leases[usize::from(registry.lease_count)] = Some(PeerLease {
                peer_id: peer.peer_id,
                user_id: peer.user_id,
                seat_mask,
                disconnected_at: None,
                permanent_bot_since: None,
                pending_reclaim: None,
                next_attempt_id: 1,
                last_transition_tick: SimTick::ZERO,
            });
            registry.lease_count += 1;
        }
        Ok(registry)
    }

    pub const fn policy(&self) -> ReconnectPolicy {
        self.policy
    }

    pub fn record_disconnect(
        &mut self,
        peer_id: PeerId,
        tick: SimTick,
    ) -> Result<(), ReconnectError> {
        let lease = self.lease_mut(peer_id)?;
        if lease.disconnected_at.is_some() {
            return Err(ReconnectError::AlreadyDisconnected);
        }
        if tick.get() < lease.last_transition_tick.get() {
            return Err(ReconnectError::TimelineRegression);
        }
        lease.disconnected_at = Some(tick);
        lease.permanent_bot_since = None;
        lease.pending_reclaim = None;
        lease.last_transition_tick = tick;
        Ok(())
    }

    pub fn substitute_control(
        &self,
        peer_id: PeerId,
        tick: SimTick,
    ) -> Result<SubstituteControl, ReconnectError> {
        let lease = self.lease(peer_id)?;
        if let Some(permanent_bot_since) = lease.permanent_bot_since {
            if tick.get() < permanent_bot_since.get() {
                return Err(ReconnectError::TimelineRegression);
            }
            return Ok(SubstituteControl::PermanentBotReplacement);
        }
        let Some(disconnected_at) = lease.disconnected_at else {
            return Ok(SubstituteControl::Connected);
        };
        if tick.get() < disconnected_at.get() {
            return Err(ReconnectError::TimelineRegression);
        }
        let elapsed = tick.get().saturating_sub(disconnected_at.get());
        if elapsed < u64::from(self.policy.neutral_input_ticks) {
            Ok(SubstituteControl::NeutralInput)
        } else if elapsed < u64::from(self.policy.grace_ticks) {
            Ok(SubstituteControl::BotTakeover)
        } else {
            Ok(SubstituteControl::PermanentBotReplacement)
        }
    }

    /// Advances the reconnect policy at `tick` and returns both the current
    /// substitute controller and the optional one-shot permanent-replacement
    /// transition. Calling this repeatedly is idempotent.
    pub fn advance_substitute_control(
        &mut self,
        peer_id: PeerId,
        tick: SimTick,
    ) -> Result<SubstituteControlResolution, ReconnectError> {
        let permanent_bot_replacement = self.finalize_grace_expiry(peer_id, tick)?;
        Ok(SubstituteControlResolution {
            control: self.substitute_control(peer_id, tick)?,
            permanent_bot_replacement,
        })
    }

    fn finalize_grace_expiry(
        &mut self,
        peer_id: PeerId,
        tick: SimTick,
    ) -> Result<Option<PermanentBotReplacement>, ReconnectError> {
        let grace_ticks = self.policy.grace_ticks;
        let lease = self.lease_mut(peer_id)?;
        if let Some(permanent_bot_since) = lease.permanent_bot_since {
            if tick.get() < permanent_bot_since.get() {
                return Err(ReconnectError::TimelineRegression);
            }
            return Ok(None);
        }
        let Some(disconnected_at) = lease.disconnected_at else {
            return Ok(None);
        };
        if tick.get() < disconnected_at.get() {
            return Err(ReconnectError::TimelineRegression);
        }
        let elapsed = tick.get().saturating_sub(disconnected_at.get());
        if elapsed < u64::from(grace_ticks) {
            return Ok(None);
        }
        let effective_tick = SimTick(
            disconnected_at
                .get()
                .checked_add(u64::from(grace_ticks))
                .ok_or(ReconnectError::TimelineExhausted)?,
        );

        lease.permanent_bot_since = Some(effective_tick);
        lease.pending_reclaim = None;
        lease.last_transition_tick = effective_tick;
        Ok(Some(PermanentBotReplacement {
            peer_id: lease.peer_id,
            seat_mask: lease.seat_mask,
            effective_tick,
        }))
    }

    /// Reserves a retained peer after the platform adapter authenticates
    /// `user_id`. This does not reconnect the lease. The authority must send the
    /// declared snapshot plus recent canonical inputs, then call
    /// [`Self::complete_reclaim`] only after receiving the matching sync
    /// acknowledgement. An interrupted transfer calls [`Self::abort_reclaim`].
    pub fn begin_reclaim(
        &mut self,
        user_id: AuthenticatedUserId,
        claim: ReconnectClaim,
        authority_tick: SimTick,
    ) -> Result<ReclaimReservation, ReconnectError> {
        if claim.match_id != self.match_id {
            return Err(ReconnectError::MatchMismatch);
        }
        if claim.last_confirmed_tick.get() > authority_tick.get() {
            return Err(ReconnectError::ConfirmedTickAhead);
        }
        if self
            .advance_substitute_control(claim.peer_id, authority_tick)?
            .control
            == SubstituteControl::PermanentBotReplacement
        {
            return Err(ReconnectError::GraceExpired);
        }
        let lease = self.lease_mut(claim.peer_id)?;
        if lease.user_id != user_id {
            return Err(ReconnectError::IdentityMismatch);
        }
        if lease.disconnected_at.is_none() {
            return Err(ReconnectError::NotDisconnected);
        }
        if lease.pending_reclaim.is_some() {
            return Err(ReconnectError::ReclaimInProgress);
        }
        let attempt_id = ReconnectAttemptId(lease.next_attempt_id);
        lease.next_attempt_id = lease
            .next_attempt_id
            .checked_add(1)
            .filter(|next| *next != 0)
            .ok_or(ReconnectError::TimelineExhausted)?;
        lease.pending_reclaim = Some(PendingReclaim {
            attempt_id,
            snapshot_tick: authority_tick,
            began_at: authority_tick,
        });
        Ok(ReclaimReservation {
            attempt_id,
            peer_id: lease.peer_id,
            seat_mask: lease.seat_mask,
            snapshot_tick: authority_tick,
        })
    }

    /// Commits a reclaim transaction after the client has acknowledged the exact
    /// reserved snapshot. Input resumes on the next authority tick observed at
    /// completion, never on a stale provisional deadline chosen before transfer.
    pub fn complete_reclaim(
        &mut self,
        peer_id: PeerId,
        attempt_id: ReconnectAttemptId,
        applied_snapshot_tick: SimTick,
        authority_tick: SimTick,
    ) -> Result<ReclaimAdmission, ReconnectError> {
        if self
            .advance_substitute_control(peer_id, authority_tick)?
            .control
            == SubstituteControl::PermanentBotReplacement
        {
            return Err(ReconnectError::GraceExpired);
        }
        let lease = self.lease_mut(peer_id)?;
        let pending = lease
            .pending_reclaim
            .ok_or(ReconnectError::NoReclaimInProgress)?;
        if pending.attempt_id != attempt_id {
            return Err(ReconnectError::ReclaimAttemptMismatch);
        }
        if pending.snapshot_tick != applied_snapshot_tick {
            return Err(ReconnectError::SnapshotMismatch);
        }
        if authority_tick.get() < pending.began_at.get()
            || authority_tick.get() < lease.last_transition_tick.get()
        {
            return Err(ReconnectError::TimelineRegression);
        }
        let resume_input_tick = authority_tick
            .get()
            .checked_add(1)
            .map(SimTick)
            .ok_or(ReconnectError::TimelineExhausted)?;
        lease.disconnected_at = None;
        lease.pending_reclaim = None;
        lease.last_transition_tick = authority_tick;
        Ok(ReclaimAdmission {
            attempt_id,
            peer_id: lease.peer_id,
            seat_mask: lease.seat_mask,
            snapshot_tick: applied_snapshot_tick,
            resume_input_tick,
        })
    }

    /// Cancels only the named in-flight transfer. The original disconnect tick
    /// remains intact, so a failed sync cannot extend the grace window or
    /// accidentally restore input ownership.
    pub fn abort_reclaim(
        &mut self,
        peer_id: PeerId,
        attempt_id: ReconnectAttemptId,
        authority_tick: SimTick,
    ) -> Result<(), ReconnectError> {
        let lease = self.lease_mut(peer_id)?;
        let pending = lease
            .pending_reclaim
            .ok_or(ReconnectError::NoReclaimInProgress)?;
        if pending.attempt_id != attempt_id {
            return Err(ReconnectError::ReclaimAttemptMismatch);
        }
        if authority_tick.get() < pending.began_at.get()
            || authority_tick.get() < lease.last_transition_tick.get()
        {
            return Err(ReconnectError::TimelineRegression);
        }
        lease.pending_reclaim = None;
        Ok(())
    }

    pub fn pending_reclaim(
        &self,
        peer_id: PeerId,
    ) -> Result<Option<ReclaimReservation>, ReconnectError> {
        let lease = self.lease(peer_id)?;
        Ok(lease.pending_reclaim.map(|pending| ReclaimReservation {
            attempt_id: pending.attempt_id,
            peer_id: lease.peer_id,
            seat_mask: lease.seat_mask,
            snapshot_tick: pending.snapshot_tick,
        }))
    }

    pub fn seat_mask(&self, peer_id: PeerId) -> Result<u8, ReconnectError> {
        Ok(self.lease(peer_id)?.seat_mask)
    }

    fn lease(&self, peer_id: PeerId) -> Result<&PeerLease, ReconnectError> {
        self.leases[..usize::from(self.lease_count)]
            .iter()
            .flatten()
            .find(|lease| lease.peer_id == peer_id)
            .ok_or(ReconnectError::UnknownPeer(peer_id))
    }

    fn lease_mut(&mut self, peer_id: PeerId) -> Result<&mut PeerLease, ReconnectError> {
        self.leases[..usize::from(self.lease_count)]
            .iter_mut()
            .flatten()
            .find(|lease| lease.peer_id == peer_id)
            .ok_or(ReconnectError::UnknownPeer(peer_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network_protocol::{FighterId, SeatAssignment, SeatId};

    fn peer(value: u64) -> PeerId {
        PeerId::new(value).unwrap()
    }

    fn user(value: u64) -> AuthenticatedUserId {
        AuthenticatedUserId::new(value).unwrap()
    }

    fn match_id(value: u8) -> MatchId {
        MatchId::new([value; 16]).unwrap()
    }

    fn ownership() -> SeatOwnership {
        SeatOwnership::from_assignments(&[
            SeatAssignment {
                seat: SeatId::new(0).unwrap(),
                fighter: FighterId::new(0).unwrap(),
                owner: SeatOwner::Peer(peer(10)),
            },
            SeatAssignment {
                seat: SeatId::new(1).unwrap(),
                fighter: FighterId::new(1).unwrap(),
                owner: SeatOwner::Peer(peer(10)),
            },
            SeatAssignment {
                seat: SeatId::new(2).unwrap(),
                fighter: FighterId::new(2).unwrap(),
                owner: SeatOwner::Peer(peer(20)),
            },
            SeatAssignment {
                seat: SeatId::new(3).unwrap(),
                fighter: FighterId::new(3).unwrap(),
                owner: SeatOwner::AuthorityBot,
            },
        ])
        .unwrap()
    }

    fn registry() -> ReconnectRegistry {
        ReconnectRegistry::new(
            match_id(7),
            &ownership(),
            &[
                AuthenticatedPeer {
                    peer_id: peer(10),
                    user_id: user(100),
                },
                AuthenticatedPeer {
                    peer_id: peer(20),
                    user_id: user(200),
                },
            ],
            ReconnectPolicy::default(),
        )
        .unwrap()
    }

    #[test]
    fn couch_peer_retains_its_exact_seat_mask() {
        let registry = registry();
        assert_eq!(registry.seat_mask(peer(10)).unwrap(), 0b0011);
        assert_eq!(registry.seat_mask(peer(20)).unwrap(), 0b0100);
    }

    #[test]
    fn neutral_then_temporary_bot_then_permanent_bot_boundaries_are_tick_exact() {
        let mut registry = registry();
        registry
            .record_disconnect(peer(10), SimTick(1_000))
            .unwrap();
        assert_eq!(
            registry.substitute_control(peer(10), SimTick(1_119)),
            Ok(SubstituteControl::NeutralInput)
        );
        assert_eq!(
            registry.substitute_control(peer(10), SimTick(1_120)),
            Ok(SubstituteControl::BotTakeover)
        );
        assert_eq!(
            registry.substitute_control(peer(10), SimTick(1_899)),
            Ok(SubstituteControl::BotTakeover)
        );
        assert_eq!(
            registry.substitute_control(peer(10), SimTick(1_900)),
            Ok(SubstituteControl::PermanentBotReplacement)
        );
    }

    #[test]
    fn permanent_bot_replacement_transition_is_emitted_once_and_clears_reclaim() {
        let mut registry = registry();
        registry
            .record_disconnect(peer(10), SimTick(1_000))
            .unwrap();
        let reservation = registry
            .begin_reclaim(
                user(100),
                ReconnectClaim {
                    match_id: match_id(7),
                    peer_id: peer(10),
                    last_confirmed_tick: SimTick(1_100),
                },
                SimTick(1_200),
            )
            .unwrap();
        assert_eq!(
            registry
                .advance_substitute_control(peer(10), SimTick(1_899))
                .unwrap(),
            SubstituteControlResolution {
                control: SubstituteControl::BotTakeover,
                permanent_bot_replacement: None,
            }
        );

        let replacement = PermanentBotReplacement {
            peer_id: peer(10),
            seat_mask: 0b0011,
            effective_tick: SimTick(1_900),
        };
        assert_eq!(
            registry
                .advance_substitute_control(peer(10), SimTick(1_905))
                .unwrap(),
            SubstituteControlResolution {
                control: SubstituteControl::PermanentBotReplacement,
                permanent_bot_replacement: Some(replacement),
            }
        );
        assert!(registry.pending_reclaim(peer(10)).unwrap().is_none());
        assert_eq!(
            registry
                .advance_substitute_control(peer(10), SimTick(2_000))
                .unwrap(),
            SubstituteControlResolution {
                control: SubstituteControl::PermanentBotReplacement,
                permanent_bot_replacement: None,
            }
        );
        assert_eq!(
            registry.complete_reclaim(
                peer(10),
                reservation.attempt_id,
                reservation.snapshot_tick,
                SimTick(2_000),
            ),
            Err(ReconnectError::GraceExpired)
        );
    }

    #[test]
    fn same_authenticated_identity_reclaims_only_after_exact_sync_ack() {
        let mut registry = registry();
        registry.record_disconnect(peer(10), SimTick(300)).unwrap();
        let reservation = registry
            .begin_reclaim(
                user(100),
                ReconnectClaim {
                    match_id: match_id(7),
                    peer_id: peer(10),
                    last_confirmed_tick: SimTick(320),
                },
                SimTick(400),
            )
            .unwrap();
        assert_eq!(reservation.seat_mask, 0b0011);
        assert_eq!(reservation.snapshot_tick, SimTick(400));
        assert_eq!(reservation.attempt_id.get(), 1);
        assert_eq!(
            registry.substitute_control(peer(10), SimTick(401)),
            Ok(SubstituteControl::NeutralInput)
        );
        let admission = registry
            .complete_reclaim(
                peer(10),
                reservation.attempt_id,
                reservation.snapshot_tick,
                SimTick(405),
            )
            .unwrap();
        assert_eq!(admission.seat_mask, 0b0011);
        assert_eq!(admission.snapshot_tick, SimTick(400));
        assert_eq!(admission.resume_input_tick, SimTick(406));
        assert_eq!(
            registry.substitute_control(peer(10), SimTick(406)),
            Ok(SubstituteControl::Connected)
        );
    }

    #[test]
    fn failed_or_aborted_sync_never_restores_or_extends_the_lease() {
        let mut registry = registry();
        registry.record_disconnect(peer(10), SimTick(300)).unwrap();
        let claim = ReconnectClaim {
            match_id: match_id(7),
            peer_id: peer(10),
            last_confirmed_tick: SimTick(320),
        };
        let first = registry
            .begin_reclaim(user(100), claim, SimTick(400))
            .unwrap();
        assert_eq!(
            registry.complete_reclaim(peer(10), first.attempt_id, SimTick(399), SimTick(405),),
            Err(ReconnectError::SnapshotMismatch)
        );
        assert_eq!(
            registry.substitute_control(peer(10), SimTick(405)),
            Ok(SubstituteControl::NeutralInput)
        );
        assert_eq!(
            registry.begin_reclaim(user(100), claim, SimTick(405)),
            Err(ReconnectError::ReclaimInProgress)
        );
        registry
            .abort_reclaim(peer(10), first.attempt_id, SimTick(406))
            .unwrap();
        assert!(registry.pending_reclaim(peer(10)).unwrap().is_none());
        let second = registry
            .begin_reclaim(user(100), claim, SimTick(407))
            .unwrap();
        assert_eq!(second.attempt_id.get(), 2);
        assert_eq!(second.snapshot_tick, SimTick(407));
    }

    #[test]
    fn wrong_attempt_and_grace_expiry_during_sync_fail_closed() {
        let mut registry = registry();
        registry.record_disconnect(peer(10), SimTick(100)).unwrap();
        let reservation = registry
            .begin_reclaim(
                user(100),
                ReconnectClaim {
                    match_id: match_id(7),
                    peer_id: peer(10),
                    last_confirmed_tick: SimTick(100),
                },
                SimTick(200),
            )
            .unwrap();
        assert_eq!(
            registry.complete_reclaim(
                peer(10),
                ReconnectAttemptId(reservation.attempt_id.get() + 1),
                reservation.snapshot_tick,
                SimTick(201),
            ),
            Err(ReconnectError::ReclaimAttemptMismatch)
        );
        assert_eq!(
            registry.complete_reclaim(
                peer(10),
                reservation.attempt_id,
                reservation.snapshot_tick,
                SimTick(100 + u64::from(CASUAL_RECONNECT_GRACE_TICKS)),
            ),
            Err(ReconnectError::GraceExpired)
        );
        assert!(registry.pending_reclaim(peer(10)).unwrap().is_none());
        assert_eq!(
            registry.substitute_control(
                peer(10),
                SimTick(100 + u64::from(CASUAL_RECONNECT_GRACE_TICKS)),
            ),
            Ok(SubstituteControl::PermanentBotReplacement)
        );
    }

    #[test]
    fn wrong_identity_match_future_claim_and_expired_claim_fail_closed() {
        let mut wrong_identity = registry();
        wrong_identity
            .record_disconnect(peer(10), SimTick(100))
            .unwrap();
        let claim = ReconnectClaim {
            match_id: match_id(7),
            peer_id: peer(10),
            last_confirmed_tick: SimTick(100),
        };
        assert_eq!(
            wrong_identity.begin_reclaim(user(999), claim, SimTick(101)),
            Err(ReconnectError::IdentityMismatch)
        );

        let mut wrong_match = registry();
        wrong_match
            .record_disconnect(peer(10), SimTick(100))
            .unwrap();
        assert_eq!(
            wrong_match.begin_reclaim(
                user(100),
                ReconnectClaim {
                    match_id: match_id(8),
                    ..claim
                },
                SimTick(101),
            ),
            Err(ReconnectError::MatchMismatch)
        );

        let mut future = registry();
        future.record_disconnect(peer(10), SimTick(100)).unwrap();
        assert_eq!(
            future.begin_reclaim(
                user(100),
                ReconnectClaim {
                    last_confirmed_tick: SimTick(500),
                    ..claim
                },
                SimTick(499),
            ),
            Err(ReconnectError::ConfirmedTickAhead)
        );

        let mut expired = registry();
        expired.record_disconnect(peer(10), SimTick(100)).unwrap();
        assert_eq!(
            expired.begin_reclaim(
                user(100),
                claim,
                SimTick(100 + u64::from(CASUAL_RECONNECT_GRACE_TICKS)),
            ),
            Err(ReconnectError::GraceExpired)
        );
    }

    #[test]
    fn roster_requires_unique_identity_for_every_owned_peer() {
        let duplicate = ReconnectRegistry::new(
            match_id(7),
            &ownership(),
            &[
                AuthenticatedPeer {
                    peer_id: peer(10),
                    user_id: user(100),
                },
                AuthenticatedPeer {
                    peer_id: peer(20),
                    user_id: user(100),
                },
            ],
            ReconnectPolicy::default(),
        );
        assert!(matches!(duplicate, Err(ReconnectError::DuplicateIdentity)));

        let missing = ReconnectRegistry::new(
            match_id(7),
            &ownership(),
            &[AuthenticatedPeer {
                peer_id: peer(10),
                user_id: user(100),
            }],
            ReconnectPolicy::default(),
        );
        assert!(matches!(
            missing,
            Err(ReconnectError::MissingIdentity(missing_peer)) if missing_peer == peer(20)
        ));
    }

    #[test]
    fn reconnect_timeline_never_moves_backwards() {
        let mut registry = registry();
        registry.record_disconnect(peer(10), SimTick(500)).unwrap();
        assert_eq!(
            registry.substitute_control(peer(10), SimTick(499)),
            Err(ReconnectError::TimelineRegression)
        );
        assert_eq!(
            registry.begin_reclaim(
                user(100),
                ReconnectClaim {
                    match_id: match_id(7),
                    peer_id: peer(10),
                    last_confirmed_tick: SimTick(450),
                },
                SimTick(499),
            ),
            Err(ReconnectError::TimelineRegression)
        );
    }
}
