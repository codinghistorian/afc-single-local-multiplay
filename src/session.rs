//! Explicit AFC client/session lifecycle and tick-based timeout policy.
//!
//! This state machine is transport-independent. Steam lobbies, UDP, and local
//! loopback feed it the same validated protocol events; none can skip manifest,
//! initial-sync, or readiness gates.

use crate::network_protocol::{
    ClockProbeId, CompatibilityId, ConnectionPhase, DisconnectCode, DisconnectMessage, MAX_SEATS,
    ManifestHash, MatchId, MatchManifest, PeerId, ProtocolValidationError, RetryDisposition,
    SimTick, StartMessage, StateHash,
};
use crate::session_clock::MIN_CLOCK_SYNC_SAMPLES;

pub const DEFAULT_CONNECT_TIMEOUT_TICKS: u32 = 600;
pub const DEFAULT_AUTH_TIMEOUT_TICKS: u32 = 600;
pub const DEFAULT_MANIFEST_TIMEOUT_TICKS: u32 = 600;
pub const DEFAULT_LOAD_TIMEOUT_TICKS: u32 = 1_800;
pub const DEFAULT_SYNC_TIMEOUT_TICKS: u32 = 600;
pub const DEFAULT_READY_TIMEOUT_TICKS: u32 = 1_800;
pub const DEFAULT_RESULT_TIMEOUT_TICKS: u32 = 600;
/// Two seconds at the fixed 60 Hz network clock gives a reliable countdown
/// enough delivery margin without making a normal startup feel sluggish.
pub const DEFAULT_COUNTDOWN_LEAD_TICKS: u32 = 120;
/// Configuration is deliberately bounded so a bad lobby value cannot hold a
/// successfully loaded match in countdown indefinitely.
pub const MAX_COUNTDOWN_LEAD_TICKS: u32 = 600;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionTimeouts {
    pub connecting: u32,
    pub authenticating: u32,
    pub manifest_agreement: u32,
    pub loading: u32,
    pub initial_sync: u32,
    pub ready: u32,
    pub confirming_result: u32,
}

impl Default for SessionTimeouts {
    fn default() -> Self {
        Self {
            connecting: DEFAULT_CONNECT_TIMEOUT_TICKS,
            authenticating: DEFAULT_AUTH_TIMEOUT_TICKS,
            manifest_agreement: DEFAULT_MANIFEST_TIMEOUT_TICKS,
            loading: DEFAULT_LOAD_TIMEOUT_TICKS,
            initial_sync: DEFAULT_SYNC_TIMEOUT_TICKS,
            ready: DEFAULT_READY_TIMEOUT_TICKS,
            confirming_result: DEFAULT_RESULT_TIMEOUT_TICKS,
        }
    }
}

impl SessionTimeouts {
    pub const fn validate(self) -> bool {
        self.connecting > 0
            && self.authenticating > 0
            && self.manifest_agreement > 0
            && self.loading > 0
            && self.initial_sync > 0
            && self.ready > 0
            && self.confirming_result > 0
    }

    const fn for_phase(self, phase: ConnectionPhase) -> Option<u32> {
        match phase {
            ConnectionPhase::Connecting => Some(self.connecting),
            ConnectionPhase::Authenticating => Some(self.authenticating),
            ConnectionPhase::ManifestAgreement => Some(self.manifest_agreement),
            ConnectionPhase::Loading => Some(self.loading),
            ConnectionPhase::InitialSync => Some(self.initial_sync),
            ConnectionPhase::Ready => Some(self.ready),
            ConnectionPhase::ConfirmingResult => Some(self.confirming_result),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionError {
    InvalidTimeoutPolicy,
    InvalidTransition {
        from: ConnectionPhase,
        to: ConnectionPhase,
    },
    Protocol(ProtocolValidationError),
    CompatibilityMismatch,
    PeerMismatch,
    MissingManifest,
    ManifestMismatch,
    SnapshotAfterStart,
    ClockNotSynchronized,
    StartTickMismatch,
    ResultBeforeFight,
    ResultIdZero,
    TimelineExhausted,
    SessionFailed,
}

impl From<ProtocolValidationError> for SessionError {
    fn from(error: ProtocolValidationError) -> Self {
        Self::Protocol(error)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppliedInitialSync {
    pub tick: SimTick,
    pub hash: StateHash,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConfirmedSessionResult {
    pub result_id: u64,
    pub final_tick: SimTick,
    pub final_hash: StateHash,
}

/// Client view of one connection. It contains protocol identities only—socket,
/// Steam callback, UI, and asset values stay at their platform boundaries.
#[derive(Clone, Copy, Debug)]
pub struct ClientSession {
    expected_compatibility: CompatibilityId,
    timeouts: SessionTimeouts,
    reconnect_bootstrap: bool,
    phase: ConnectionPhase,
    entered_tick: SimTick,
    deadline_tick: Option<SimTick>,
    peer_id: Option<PeerId>,
    manifest: Option<MatchManifest>,
    initial_sync: Option<AppliedInitialSync>,
    clock_synchronized: bool,
    countdown_start_tick: Option<SimTick>,
    result: Option<ConfirmedSessionResult>,
    failure: Option<DisconnectMessage>,
}

impl ClientSession {
    pub fn new(
        expected_compatibility: CompatibilityId,
        timeouts: SessionTimeouts,
        tick: SimTick,
    ) -> Result<Self, SessionError> {
        expected_compatibility.validate()?;
        if !timeouts.validate() {
            return Err(SessionError::InvalidTimeoutPolicy);
        }
        Ok(Self {
            expected_compatibility,
            timeouts,
            reconnect_bootstrap: false,
            phase: ConnectionPhase::OfflineMenu,
            entered_tick: tick,
            deadline_tick: None,
            peer_id: None,
            manifest: None,
            initial_sync: None,
            clock_synchronized: false,
            countdown_start_tick: None,
            result: None,
            failure: None,
        })
    }

    /// Restores the immutable agreement needed by a replacement transport.
    /// Reconnect skips lobby/manifest/countdown negotiation, but remains in the
    /// bounded sync phase until a verified snapshot and fresh clock samples are
    /// complete.
    pub fn new_reconnect(
        expected_compatibility: CompatibilityId,
        timeouts: SessionTimeouts,
        peer_id: PeerId,
        manifest: MatchManifest,
        countdown_start_tick: SimTick,
        tick: SimTick,
    ) -> Result<Self, SessionError> {
        manifest.validate()?;
        manifest
            .compatibility
            .validate_against(&expected_compatibility)
            .map_err(|_| SessionError::CompatibilityMismatch)?;
        peer_id.validate()?;
        if !manifest.ownership.peer_owns_any_seat(peer_id) {
            return Err(SessionError::PeerMismatch);
        }
        StartMessage::Countdown {
            match_id: manifest.match_id,
            start_tick: countdown_start_tick,
        }
        .validate_against_manifest(&manifest)?;

        let mut session = Self::new(expected_compatibility, timeouts, tick)?;
        session.reconnect_bootstrap = true;
        session.phase = ConnectionPhase::InitialSync;
        session.entered_tick = tick;
        session.deadline_tick = Some(SimTick(
            tick.0
                .checked_add(u64::from(timeouts.initial_sync))
                .ok_or(SessionError::TimelineExhausted)?,
        ));
        session.peer_id = Some(peer_id);
        session.manifest = Some(manifest);
        session.countdown_start_tick = Some(countdown_start_tick);
        Ok(session)
    }

    pub const fn phase(&self) -> ConnectionPhase {
        self.phase
    }

    /// True only while a replacement transport is completing its snapshot and
    /// fresh-clock gate. Results may be retained at the reliable channel head in
    /// this phase; ordinary startup never receives that exception.
    pub const fn is_reconnect_initial_sync(&self) -> bool {
        self.reconnect_bootstrap && matches!(self.phase, ConnectionPhase::InitialSync)
    }

    pub const fn peer_id(&self) -> Option<PeerId> {
        self.peer_id
    }

    pub const fn manifest(&self) -> Option<&MatchManifest> {
        self.manifest.as_ref()
    }

    pub const fn initial_sync(&self) -> Option<AppliedInitialSync> {
        self.initial_sync
    }

    pub const fn is_clock_synchronized(&self) -> bool {
        self.clock_synchronized
    }

    /// The authority-selected gameplay boundary from `StartMessage::Countdown`.
    /// The manifest's `agreed_start_tick` is only an earliest proposal.
    pub const fn countdown_start_tick(&self) -> Option<SimTick> {
        self.countdown_start_tick
    }

    pub const fn result(&self) -> Option<ConfirmedSessionResult> {
        self.result
    }

    pub const fn deadline_tick(&self) -> Option<SimTick> {
        self.deadline_tick
    }

    pub const fn failure(&self) -> Option<DisconnectMessage> {
        self.failure
    }

    pub fn enter_lobby(&mut self, tick: SimTick) -> Result<(), SessionError> {
        self.transition(ConnectionPhase::Lobby, tick)?;
        self.clear_match();
        Ok(())
    }

    pub fn start_connecting(&mut self, tick: SimTick) -> Result<(), SessionError> {
        self.transition(ConnectionPhase::Connecting, tick)
    }

    pub fn transport_connected(&mut self, tick: SimTick) -> Result<(), SessionError> {
        self.transition(ConnectionPhase::Authenticating, tick)
    }

    pub fn authentication_succeeded(
        &mut self,
        peer_id: PeerId,
        tick: SimTick,
    ) -> Result<(), SessionError> {
        peer_id.validate()?;
        self.transition(ConnectionPhase::ManifestAgreement, tick)?;
        self.peer_id = Some(peer_id);
        Ok(())
    }

    pub fn accept_manifest(
        &mut self,
        manifest: MatchManifest,
        tick: SimTick,
    ) -> Result<StartMessage, SessionError> {
        if self.phase != ConnectionPhase::ManifestAgreement {
            return Err(SessionError::InvalidTransition {
                from: self.phase,
                to: ConnectionPhase::Loading,
            });
        }
        // Loading may legitimately finish after the manifest's proposed start
        // tick. The final boundary is selected only after all peers are ready.
        manifest.validate()?;
        manifest
            .compatibility
            .validate_against(&self.expected_compatibility)
            .map_err(|_| SessionError::CompatibilityMismatch)?;
        let peer_id = self.peer_id.ok_or(SessionError::PeerMismatch)?;
        if !manifest.ownership.peer_owns_any_seat(peer_id) {
            return Err(SessionError::PeerMismatch);
        }
        self.transition(ConnectionPhase::Loading, tick)?;
        self.manifest = Some(manifest);
        Ok(StartMessage::ManifestAccepted {
            match_id: manifest.match_id,
            peer_id,
            manifest_hash: manifest.manifest_hash,
        })
    }

    pub fn content_loaded(&mut self, tick: SimTick) -> Result<(), SessionError> {
        if self.manifest.is_none() {
            return Err(SessionError::MissingManifest);
        }
        self.transition(ConnectionPhase::InitialSync, tick)
    }

    pub fn apply_initial_sync(
        &mut self,
        match_id: MatchId,
        snapshot_tick: SimTick,
        snapshot_hash: StateHash,
        tick: SimTick,
    ) -> Result<StartMessage, SessionError> {
        let manifest = self.manifest.ok_or(SessionError::MissingManifest)?;
        if match_id != manifest.match_id {
            return Err(SessionError::ManifestMismatch);
        }
        let peer_id = self.peer_id.ok_or(SessionError::PeerMismatch)?;
        self.transition(ConnectionPhase::Ready, tick)?;
        self.initial_sync = Some(AppliedInitialSync {
            tick: snapshot_tick,
            hash: snapshot_hash,
        });
        Ok(StartMessage::InitialSyncApplied {
            match_id,
            peer_id,
            snapshot_tick,
            snapshot_hash,
        })
    }

    pub fn ready_message(&self) -> Result<StartMessage, SessionError> {
        if self.phase != ConnectionPhase::Ready || self.initial_sync.is_none() {
            return Err(SessionError::InvalidTransition {
                from: self.phase,
                to: ConnectionPhase::Countdown,
            });
        }
        if !self.clock_synchronized {
            return Err(SessionError::ClockNotSynchronized);
        }
        let manifest = self.manifest.ok_or(SessionError::MissingManifest)?;
        Ok(StartMessage::Ready {
            match_id: manifest.match_id,
            peer_id: self.peer_id.ok_or(SessionError::PeerMismatch)?,
        })
    }

    /// Marks the platform/session clock estimator ready. This acknowledgement is
    /// deliberately separate from snapshot application so a remote client cannot
    /// enter countdown using an unsynchronized render/network clock.
    pub fn mark_clock_synchronized(&mut self) -> Result<(), SessionError> {
        let initial_startup_ready =
            self.phase == ConnectionPhase::Ready && self.initial_sync.is_some();
        let reconnect_sync = self.phase == ConnectionPhase::InitialSync
            && self.manifest.is_some()
            && self.countdown_start_tick.is_some();
        if !initial_startup_ready && !reconnect_sync {
            return Err(SessionError::InvalidTransition {
                from: self.phase,
                to: ConnectionPhase::Ready,
            });
        }
        self.clock_synchronized = true;
        Ok(())
    }

    pub fn begin_countdown(
        &mut self,
        message: StartMessage,
        tick: SimTick,
    ) -> Result<(), SessionError> {
        let manifest = self.manifest.ok_or(SessionError::MissingManifest)?;
        if !self.clock_synchronized {
            return Err(SessionError::ClockNotSynchronized);
        }
        message.validate_against_manifest(&manifest)?;
        let StartMessage::Countdown { start_tick, .. } = message else {
            return Err(SessionError::StartTickMismatch);
        };
        if start_tick.0 <= tick.0 || start_tick < manifest.agreed_start_tick {
            return Err(SessionError::StartTickMismatch);
        }
        self.transition(ConnectionPhase::Countdown, tick)?;
        self.countdown_start_tick = Some(start_tick);
        Ok(())
    }

    /// Completes the reconnect-only InitialSync -> Fighting transition after
    /// the replacement client has acknowledged the snapshot and synchronized
    /// its authority clock. Normal startup cannot call this shortcut.
    pub fn complete_reconnect(
        &mut self,
        sync: AppliedInitialSync,
        tick: SimTick,
    ) -> Result<(), SessionError> {
        if self.phase != ConnectionPhase::InitialSync
            || self.manifest.is_none()
            || self.countdown_start_tick.is_none()
            || !self.clock_synchronized
        {
            return Err(SessionError::InvalidTransition {
                from: self.phase,
                to: ConnectionPhase::Fighting,
            });
        }
        self.phase = ConnectionPhase::Fighting;
        self.entered_tick = tick;
        self.deadline_tick = None;
        self.initial_sync = Some(sync);
        Ok(())
    }

    /// Advances countdown at the canonical network tick. It never reads elapsed
    /// wall time, and enters fighting on the authority-selected start tick.
    pub fn observe_tick(&mut self, tick: SimTick) -> Result<bool, SessionError> {
        if self.failure.is_some() {
            return Err(SessionError::SessionFailed);
        }
        if self.phase == ConnectionPhase::Countdown {
            let start = self
                .countdown_start_tick
                .ok_or(SessionError::StartTickMismatch)?;
            if tick.0 >= start.0 {
                self.transition(ConnectionPhase::Fighting, start)?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn begin_result_confirmation(&mut self, tick: SimTick) -> Result<(), SessionError> {
        self.transition(ConnectionPhase::ConfirmingResult, tick)
    }

    pub fn accept_confirmed_result(
        &mut self,
        match_id: MatchId,
        result: ConfirmedSessionResult,
        tick: SimTick,
    ) -> Result<(), SessionError> {
        if self.phase != ConnectionPhase::ConfirmingResult {
            return Err(SessionError::ResultBeforeFight);
        }
        if result.result_id == 0 {
            return Err(SessionError::ResultIdZero);
        }
        if self.manifest.map(|manifest| manifest.match_id) != Some(match_id) {
            return Err(SessionError::ManifestMismatch);
        }
        self.transition(ConnectionPhase::Results, tick)?;
        self.result = Some(result);
        Ok(())
    }

    pub fn return_to_lobby(&mut self, tick: SimTick) -> Result<(), SessionError> {
        self.transition(ConnectionPhase::Lobby, tick)?;
        self.clear_match();
        Ok(())
    }

    /// Produces one stable disconnect reason on the first expired tick.
    pub fn check_timeout(&mut self, tick: SimTick) -> Option<DisconnectMessage> {
        if let Some(failure) = self.failure {
            return Some(failure);
        }
        let deadline = self.deadline_tick?;
        if tick.0 < deadline.0 {
            return None;
        }
        let failure = DisconnectMessage {
            match_id: self.manifest.map(|manifest| manifest.match_id),
            code: DisconnectCode::Timeout,
            retry: if matches!(
                self.phase,
                ConnectionPhase::Fighting | ConnectionPhase::ConfirmingResult
            ) {
                RetryDisposition::ReconnectAllowed
            } else {
                RetryDisposition::ReturnToLobby
            },
            detail_code: timeout_detail_code(self.phase),
            last_confirmed_tick: self.initial_sync.map(|sync| sync.tick),
        };
        self.failure = Some(failure);
        Some(failure)
    }

    fn transition(&mut self, next: ConnectionPhase, tick: SimTick) -> Result<(), SessionError> {
        if self.failure.is_some() {
            return Err(SessionError::SessionFailed);
        }
        if !self.phase.can_transition_to(next) {
            return Err(SessionError::InvalidTransition {
                from: self.phase,
                to: next,
            });
        }
        self.phase = next;
        self.entered_tick = tick;
        self.deadline_tick = match self.timeouts.for_phase(next) {
            Some(duration) => Some(SimTick(
                tick.0
                    .checked_add(u64::from(duration))
                    .ok_or(SessionError::TimelineExhausted)?,
            )),
            None => None,
        };
        Ok(())
    }

    fn clear_match(&mut self) {
        self.reconnect_bootstrap = false;
        self.peer_id = None;
        self.manifest = None;
        self.initial_sync = None;
        self.clock_synchronized = false;
        self.countdown_start_tick = None;
        self.result = None;
        self.failure = None;
    }
}

const fn timeout_detail_code(phase: ConnectionPhase) -> u16 {
    match phase {
        ConnectionPhase::Connecting => 100,
        ConnectionPhase::Authenticating => 101,
        ConnectionPhase::ManifestAgreement => 102,
        ConnectionPhase::Loading => 103,
        ConnectionPhase::InitialSync => 104,
        ConnectionPhase::Ready => 105,
        ConnectionPhase::ConfirmingResult => 106,
        _ => 199,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AuthorityPeerReadiness {
    pub peer_id: Option<PeerId>,
    pub authenticated: bool,
    pub manifest_hash: Option<ManifestHash>,
    pub initial_sync: Option<AppliedInitialSync>,
    pub clock_probe_count: u8,
    pub last_clock_probe: Option<ClockProbeId>,
    pub ready: bool,
}

/// Fixed-capacity authority-side agreement table. One peer may own several seats,
/// but appears only once here.
#[derive(Clone, Copy, Debug)]
pub struct AuthoritySessionGate {
    manifest: MatchManifest,
    peers: [AuthorityPeerReadiness; MAX_SEATS],
    peer_count: u8,
    countdown_start_tick: Option<SimTick>,
}

impl AuthoritySessionGate {
    pub fn new(manifest: MatchManifest) -> Result<Self, SessionError> {
        manifest.validate()?;
        let mut gate = Self {
            manifest,
            peers: [AuthorityPeerReadiness::default(); MAX_SEATS],
            peer_count: 0,
            countdown_start_tick: None,
        };
        for assignment in manifest.ownership.as_slice() {
            let crate::network_protocol::SeatOwner::Peer(peer_id) = assignment.owner else {
                continue;
            };
            if gate.peer(peer_id).is_some() {
                continue;
            }
            gate.peers[gate.peer_count as usize].peer_id = Some(peer_id);
            gate.peer_count += 1;
        }
        Ok(gate)
    }

    pub fn peer(&self, peer_id: PeerId) -> Option<&AuthorityPeerReadiness> {
        self.peers[..self.peer_count as usize]
            .iter()
            .find(|peer| peer.peer_id == Some(peer_id))
    }

    pub const fn match_id(&self) -> MatchId {
        self.manifest.match_id
    }

    pub const fn countdown_start_tick(&self) -> Option<SimTick> {
        self.countdown_start_tick
    }

    fn peer_mut(&mut self, peer_id: PeerId) -> Result<&mut AuthorityPeerReadiness, SessionError> {
        self.peers[..self.peer_count as usize]
            .iter_mut()
            .find(|peer| peer.peer_id == Some(peer_id))
            .ok_or(SessionError::PeerMismatch)
    }

    pub fn authenticate(
        &mut self,
        peer_id: PeerId,
        compatibility: CompatibilityId,
    ) -> Result<(), SessionError> {
        compatibility
            .validate_against(&self.manifest.compatibility)
            .map_err(|_| SessionError::CompatibilityMismatch)?;
        self.peer_mut(peer_id)?.authenticated = true;
        Ok(())
    }

    pub fn accept_manifest(
        &mut self,
        peer_id: PeerId,
        hash: ManifestHash,
    ) -> Result<(), SessionError> {
        if hash != self.manifest.manifest_hash {
            return Err(SessionError::ManifestMismatch);
        }
        let peer = self.peer_mut(peer_id)?;
        if !peer.authenticated {
            return Err(SessionError::InvalidTransition {
                from: ConnectionPhase::Authenticating,
                to: ConnectionPhase::ManifestAgreement,
            });
        }
        peer.manifest_hash = Some(hash);
        Ok(())
    }

    pub fn apply_initial_sync(
        &mut self,
        peer_id: PeerId,
        sync: AppliedInitialSync,
    ) -> Result<(), SessionError> {
        let expected_hash = self.manifest.manifest_hash;
        let peer = self.peer_mut(peer_id)?;
        if peer.manifest_hash != Some(expected_hash) {
            return Err(SessionError::ManifestMismatch);
        }
        peer.initial_sync = Some(sync);
        Ok(())
    }

    pub fn mark_ready(&mut self, peer_id: PeerId) -> Result<(), SessionError> {
        let peer = self.peer_mut(peer_id)?;
        if peer.initial_sync.is_none() {
            return Err(SessionError::InvalidTransition {
                from: ConnectionPhase::InitialSync,
                to: ConnectionPhase::Ready,
            });
        }
        if peer.clock_probe_count < MIN_CLOCK_SYNC_SAMPLES {
            return Err(SessionError::ClockNotSynchronized);
        }
        peer.ready = true;
        Ok(())
    }

    /// Records one unique probe for which the runtime has successfully reserved
    /// an authority reply. Reliable retransmission of the same probe is idempotent.
    pub fn observe_clock_probe(
        &mut self,
        peer_id: PeerId,
        probe_id: ClockProbeId,
    ) -> Result<bool, SessionError> {
        probe_id.validate()?;
        let peer = self.peer_mut(peer_id)?;
        if peer.last_clock_probe == Some(probe_id) {
            return Ok(false);
        }
        peer.last_clock_probe = Some(probe_id);
        peer.clock_probe_count = peer.clock_probe_count.saturating_add(1);
        Ok(true)
    }

    pub fn all_ready(&self) -> bool {
        self.peers[..self.peer_count as usize]
            .iter()
            .all(|peer| peer.ready)
    }

    /// Chooses the immutable actual start boundary once readiness is complete.
    /// Repeated calls are idempotent and return the same packet.
    pub fn begin_countdown(
        &mut self,
        now: SimTick,
        lead_ticks: u32,
    ) -> Result<StartMessage, SessionError> {
        if !self.all_ready() {
            return Err(SessionError::InvalidTransition {
                from: ConnectionPhase::Ready,
                to: ConnectionPhase::Countdown,
            });
        }
        if lead_ticks == 0 || lead_ticks > MAX_COUNTDOWN_LEAD_TICKS {
            return Err(SessionError::InvalidTimeoutPolicy);
        }
        let start_tick = match self.countdown_start_tick {
            Some(start_tick) => start_tick,
            None => {
                let after_lead = SimTick(
                    now.0
                        .checked_add(u64::from(lead_ticks))
                        .ok_or(SessionError::TimelineExhausted)?,
                );
                let start_tick = self.manifest.agreed_start_tick.max(after_lead);
                self.countdown_start_tick = Some(start_tick);
                start_tick
            }
        };
        Ok(StartMessage::Countdown {
            match_id: self.manifest.match_id,
            start_tick,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network_protocol::{
        AuthorityKind, BuildId, DefinitionId, FighterId, FighterSlotConfig, GameplayContentHash,
        ManifestHash, ProtocolVersion, ReplayFormatVersion, SeatAssignment, SeatId, SeatOwner,
        SeatOwnership, SimulationVersion, TeamId,
    };

    fn peer_id() -> PeerId {
        PeerId::new(7).unwrap()
    }

    fn compatibility() -> CompatibilityId {
        CompatibilityId {
            protocol: ProtocolVersion::new(1).unwrap(),
            simulation: SimulationVersion::new(1).unwrap(),
            replay: ReplayFormatVersion::new(1).unwrap(),
            build: BuildId::new([1; 16]).unwrap(),
            gameplay_content: GameplayContentHash::new([2; 32]).unwrap(),
        }
    }

    fn manifest() -> MatchManifest {
        let ownership = SeatOwnership::from_assignments(&[SeatAssignment {
            seat: SeatId::new(0).unwrap(),
            fighter: FighterId::new(0).unwrap(),
            owner: SeatOwner::Peer(peer_id()),
        }])
        .unwrap();
        let mut slots = [FighterSlotConfig::default(); 4];
        slots[0] = FighterSlotConfig {
            occupied: true,
            fighter: FighterId::new(0).unwrap(),
            team: TeamId::new(0).unwrap(),
            character: DefinitionId::new(1).unwrap(),
            style: DefinitionId::new(1).unwrap(),
            equipment: DefinitionId::new(0).unwrap(),
        };
        MatchManifest {
            compatibility: compatibility(),
            manifest_hash: ManifestHash(9),
            match_id: MatchId::new([3; 16]).unwrap(),
            authority: AuthorityKind::Listen,
            trusted_results: false,
            arena: DefinitionId::new(1).unwrap(),
            rules: DefinitionId::new(1).unwrap(),
            slots,
            ownership,
            master_gameplay_seed: 5,
            rng_scheme_version: 1,
            tick_rate_hz: 60,
            input_delay_ticks: 2,
            rollback_limit_ticks: 12,
            snapshot_history_ticks: 32,
            agreed_start_tick: SimTick(100),
        }
    }

    #[test]
    fn client_cannot_skip_manifest_loading_or_initial_sync() {
        let mut client =
            ClientSession::new(compatibility(), SessionTimeouts::default(), SimTick(0)).unwrap();
        client.enter_lobby(SimTick(1)).unwrap();
        client.start_connecting(SimTick(2)).unwrap();
        assert_eq!(
            client.content_loaded(SimTick(3)),
            Err(SessionError::MissingManifest)
        );
        assert!(matches!(
            client.begin_countdown(
                StartMessage::Countdown {
                    match_id: manifest().match_id,
                    start_tick: SimTick(100)
                },
                SimTick(3)
            ),
            Err(SessionError::MissingManifest)
        ));
    }

    #[test]
    fn complete_session_enters_fight_on_exact_countdown_tick() {
        let manifest = manifest();
        let mut client =
            ClientSession::new(compatibility(), SessionTimeouts::default(), SimTick(0)).unwrap();
        client.enter_lobby(SimTick(1)).unwrap();
        client.start_connecting(SimTick(2)).unwrap();
        client.transport_connected(SimTick(3)).unwrap();
        client
            .authentication_succeeded(peer_id(), SimTick(4))
            .unwrap();
        client.accept_manifest(manifest, SimTick(5)).unwrap();
        client.content_loaded(SimTick(6)).unwrap();
        client
            .apply_initial_sync(manifest.match_id, SimTick(6), StateHash(88), SimTick(7))
            .unwrap();
        client.mark_clock_synchronized().unwrap();
        client.ready_message().unwrap();
        client
            .begin_countdown(
                StartMessage::Countdown {
                    match_id: manifest.match_id,
                    start_tick: manifest.agreed_start_tick,
                },
                SimTick(8),
            )
            .unwrap();
        assert_eq!(
            client.countdown_start_tick(),
            Some(manifest.agreed_start_tick)
        );
        assert!(!client.observe_tick(SimTick(99)).unwrap());
        assert!(client.observe_tick(SimTick(100)).unwrap());
        assert_eq!(client.phase(), ConnectionPhase::Fighting);
    }

    #[test]
    fn expired_manifest_proposal_does_not_block_late_loading() {
        let manifest = manifest();
        let mut client =
            ClientSession::new(compatibility(), SessionTimeouts::default(), SimTick(0)).unwrap();
        client.enter_lobby(SimTick(1)).unwrap();
        client.start_connecting(SimTick(2)).unwrap();
        client.transport_connected(SimTick(3)).unwrap();
        client
            .authentication_succeeded(peer_id(), SimTick(4))
            .unwrap();

        client.accept_manifest(manifest, SimTick(120)).unwrap();
        client.content_loaded(SimTick(130)).unwrap();
        client
            .apply_initial_sync(manifest.match_id, SimTick(125), StateHash(88), SimTick(131))
            .unwrap();
        client.mark_clock_synchronized().unwrap();
        client
            .begin_countdown(
                StartMessage::Countdown {
                    match_id: manifest.match_id,
                    start_tick: SimTick(260),
                },
                SimTick(140),
            )
            .unwrap();

        assert_eq!(client.countdown_start_tick(), Some(SimTick(260)));
        assert!(!client.observe_tick(SimTick(259)).unwrap());
        assert!(client.observe_tick(SimTick(260)).unwrap());
    }

    #[test]
    fn reconnect_bootstrap_requires_fresh_clock_before_fighting() {
        let manifest = manifest();
        let mut client = ClientSession::new_reconnect(
            compatibility(),
            SessionTimeouts::default(),
            peer_id(),
            manifest,
            SimTick(260),
            SimTick(500),
        )
        .unwrap();
        let sync = AppliedInitialSync {
            tick: SimTick(240),
            hash: StateHash(88),
        };

        assert_eq!(client.phase(), ConnectionPhase::InitialSync);
        assert!(client.is_reconnect_initial_sync());
        assert_eq!(client.countdown_start_tick(), Some(SimTick(260)));
        assert!(matches!(
            client.complete_reconnect(sync, SimTick(501)),
            Err(SessionError::InvalidTransition { .. })
        ));
        client.mark_clock_synchronized().unwrap();
        client.complete_reconnect(sync, SimTick(502)).unwrap();
        assert_eq!(client.phase(), ConnectionPhase::Fighting);
        assert!(!client.is_reconnect_initial_sync());
        assert_eq!(client.initial_sync(), Some(sync));
        assert_eq!(client.deadline_tick(), None);
    }

    #[test]
    fn reconnect_bootstrap_exception_does_not_leak_into_the_next_match() {
        let manifest = manifest();
        let mut client = ClientSession::new_reconnect(
            compatibility(),
            SessionTimeouts::default(),
            peer_id(),
            manifest,
            SimTick(260),
            SimTick(500),
        )
        .unwrap();
        let sync = AppliedInitialSync {
            tick: SimTick(240),
            hash: StateHash(88),
        };
        client.mark_clock_synchronized().unwrap();
        client.complete_reconnect(sync, SimTick(501)).unwrap();
        client.begin_result_confirmation(SimTick(502)).unwrap();
        client
            .accept_confirmed_result(
                manifest.match_id,
                ConfirmedSessionResult {
                    result_id: 1,
                    final_tick: SimTick(250),
                    final_hash: StateHash(99),
                },
                SimTick(503),
            )
            .unwrap();
        client.return_to_lobby(SimTick(504)).unwrap();

        client.start_connecting(SimTick(505)).unwrap();
        client.transport_connected(SimTick(506)).unwrap();
        client
            .authentication_succeeded(peer_id(), SimTick(507))
            .unwrap();
        client.accept_manifest(manifest, SimTick(508)).unwrap();
        client.content_loaded(SimTick(509)).unwrap();

        assert_eq!(client.phase(), ConnectionPhase::InitialSync);
        assert!(!client.is_reconnect_initial_sync());
    }

    #[test]
    fn incompatible_manifest_is_rejected_before_loading() {
        let mut incompatible = manifest();
        incompatible.compatibility.simulation = SimulationVersion::new(2).unwrap();
        let mut client =
            ClientSession::new(compatibility(), SessionTimeouts::default(), SimTick(0)).unwrap();
        client.enter_lobby(SimTick(1)).unwrap();
        client.start_connecting(SimTick(2)).unwrap();
        client.transport_connected(SimTick(3)).unwrap();
        client
            .authentication_succeeded(peer_id(), SimTick(4))
            .unwrap();
        assert_eq!(
            client.accept_manifest(incompatible, SimTick(5)),
            Err(SessionError::CompatibilityMismatch)
        );
        assert_eq!(client.phase(), ConnectionPhase::ManifestAgreement);
    }

    #[test]
    fn phase_timeout_is_tick_exact_and_idempotent() {
        let mut client =
            ClientSession::new(compatibility(), SessionTimeouts::default(), SimTick(0)).unwrap();
        client.enter_lobby(SimTick(1)).unwrap();
        client.start_connecting(SimTick(10)).unwrap();
        let deadline = SimTick(10 + u64::from(DEFAULT_CONNECT_TIMEOUT_TICKS));
        assert!(client.check_timeout(deadline.wrapping_sub(1)).is_none());
        let first = client.check_timeout(deadline).unwrap();
        assert_eq!(first.code, DisconnectCode::Timeout);
        assert_eq!(first.detail_code, 100);
        assert_eq!(client.check_timeout(deadline.wrapping_add(5)), Some(first));
    }

    #[test]
    fn authority_gate_requires_auth_manifest_sync_and_ready_in_order() {
        let manifest = manifest();
        let mut gate = AuthoritySessionGate::new(manifest).unwrap();
        assert!(!gate.all_ready());
        assert!(gate.mark_ready(peer_id()).is_err());
        gate.authenticate(peer_id(), compatibility()).unwrap();
        gate.accept_manifest(peer_id(), manifest.manifest_hash)
            .unwrap();
        gate.apply_initial_sync(
            peer_id(),
            AppliedInitialSync {
                tick: SimTick(10),
                hash: StateHash(20),
            },
        )
        .unwrap();
        for probe in 1..=u32::from(MIN_CLOCK_SYNC_SAMPLES) {
            gate.observe_clock_probe(peer_id(), ClockProbeId::new(probe).unwrap())
                .unwrap();
        }
        gate.mark_ready(peer_id()).unwrap();
        assert!(gate.all_ready());
        assert_eq!(
            gate.begin_countdown(SimTick(150), DEFAULT_COUNTDOWN_LEAD_TICKS)
                .unwrap(),
            StartMessage::Countdown {
                match_id: manifest.match_id,
                start_tick: SimTick(270)
            }
        );
        assert_eq!(gate.countdown_start_tick(), Some(SimTick(270)));
        assert_eq!(
            gate.begin_countdown(SimTick(200), DEFAULT_COUNTDOWN_LEAD_TICKS)
                .unwrap(),
            StartMessage::Countdown {
                match_id: manifest.match_id,
                start_tick: SimTick(270)
            }
        );
    }

    #[test]
    fn final_result_is_authority_confirmed_and_idempotently_stored() {
        let manifest = manifest();
        let mut client =
            ClientSession::new(compatibility(), SessionTimeouts::default(), SimTick(0)).unwrap();
        client.enter_lobby(SimTick(1)).unwrap();
        client.start_connecting(SimTick(2)).unwrap();
        client.transport_connected(SimTick(3)).unwrap();
        client
            .authentication_succeeded(peer_id(), SimTick(4))
            .unwrap();
        client.accept_manifest(manifest, SimTick(5)).unwrap();
        client.content_loaded(SimTick(6)).unwrap();
        client
            .apply_initial_sync(manifest.match_id, SimTick(6), StateHash(1), SimTick(7))
            .unwrap();
        client.mark_clock_synchronized().unwrap();
        client
            .begin_countdown(
                StartMessage::Countdown {
                    match_id: manifest.match_id,
                    start_tick: SimTick(100),
                },
                SimTick(8),
            )
            .unwrap();
        client.observe_tick(SimTick(100)).unwrap();
        client.begin_result_confirmation(SimTick(200)).unwrap();
        let result = ConfirmedSessionResult {
            result_id: 9,
            final_tick: SimTick(199),
            final_hash: StateHash(44),
        };
        client
            .accept_confirmed_result(manifest.match_id, result, SimTick(201))
            .unwrap();
        assert_eq!(client.result(), Some(result));
        assert_eq!(client.phase(), ConnectionPhase::Results);
    }
}
