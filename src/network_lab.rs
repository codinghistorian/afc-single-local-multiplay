//! Bounded multi-peer transport coordination and acceptance-lab instrumentation.
//!
//! One [`NetworkRuntime`] still represents exactly one remote connection.  This
//! module supplies the authority-side fan-out that a real listen or dedicated
//! server needs: a fixed maximum of four peer links, stable peer tagging for
//! inbound events, explicit countdown consensus across the per-connection
//! session gates, and transport-level byte accounting by AFC channel.
//!
//! The coordinator deliberately does not own or step gameplay.  The canonical
//! [`crate::authority::AuthorityMatch`] remains the only authority, while callers
//! route tagged input events into it and broadcast its committed-input/state
//! messages through this type.  That separation also lets the same coordinator
//! run over ordinary UDP, the deterministic fault laboratory, and Steam SDR.

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::network_codec::{Handshake, WireMessage};
use crate::network_io::{
    AFC_CHANNEL_COUNT, AfcChannel, AfcDatagram, NonBlockingDatagramEndpoint, ReceiveOutcome,
    SendOutcome,
};
use crate::network_protocol::{
    ConnectionPhase, MAX_SEATS, MatchManifest, PeerId, ProtocolValidationError, SimTick,
    StartMessage,
};
use crate::network_runtime::{
    NetworkRuntime, PumpReport, QueueDisposition, RuntimeConfig, RuntimeConfigError, RuntimeEvent,
    RuntimeMetrics, RuntimeQueueError,
};
use crate::session::{
    AuthoritySessionGate, ClientSession, DEFAULT_COUNTDOWN_LEAD_TICKS, SessionError,
    SessionTimeouts,
};

/// AFC uses a 60 Hz canonical network clock for acceptance budgets.
pub const NETWORK_LAB_HZ: u64 = 60;
pub const MAX_NETWORK_PEERS: usize = MAX_SEATS;
pub const UPSTREAM_BUDGET_BYTES_PER_SECOND: u64 = 16 * 1024;
pub const DOWNSTREAM_BUDGET_BYTES_PER_SECOND: u64 = 64 * 1024;

const RUNTIME_MAGIC: [u8; 4] = *b"AFCR";
const RUNTIME_CHANNEL_OFFSET: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetworkAcceptanceScenario {
    NetLoopback4,
    NetTypical4,
    NetDegraded4,
    RollbackStorm,
}

impl NetworkAcceptanceScenario {
    pub const fn name(self) -> &'static str {
        match self {
            Self::NetLoopback4 => "NetLoopback4",
            Self::NetTypical4 => "NetTypical4",
            Self::NetDegraded4 => "NetDegraded4",
            Self::RollbackStorm => "RollbackStorm",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChannelTraffic {
    pub datagrams: u64,
    pub bytes: u64,
}

impl ChannelTraffic {
    fn record(&mut self, bytes: usize) {
        self.datagrams = self.datagrams.saturating_add(1);
        self.bytes = self.bytes.saturating_add(bytes as u64);
    }

    fn saturating_sub(self, earlier: Self) -> Self {
        Self {
            datagrams: self.datagrams.saturating_sub(earlier.datagrams),
            bytes: self.bytes.saturating_sub(earlier.bytes),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DirectionTraffic {
    channels: [ChannelTraffic; AFC_CHANNEL_COUNT],
    pub unclassified: ChannelTraffic,
}

impl DirectionTraffic {
    pub const fn channel(&self, channel: AfcChannel) -> ChannelTraffic {
        self.channels[channel as usize - 1]
    }

    pub fn total(self) -> ChannelTraffic {
        self.channels
            .into_iter()
            .fold(self.unclassified, |mut total, channel| {
                total.datagrams = total.datagrams.saturating_add(channel.datagrams);
                total.bytes = total.bytes.saturating_add(channel.bytes);
                total
            })
    }

    fn record(&mut self, datagram: &AfcDatagram) {
        match runtime_channel(datagram) {
            Some(channel) => self.channels[channel as usize - 1].record(datagram.len()),
            None => self.unclassified.record(datagram.len()),
        }
    }

    fn saturating_sub(self, earlier: Self) -> Self {
        Self {
            channels: std::array::from_fn(|index| {
                self.channels[index].saturating_sub(earlier.channels[index])
            }),
            unclassified: self.unclassified.saturating_sub(earlier.unclassified),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TrafficSnapshot {
    /// Datagram bytes successfully accepted by the wrapped endpoint.
    pub sent: DirectionTraffic,
    /// Datagram bytes delivered by the wrapped endpoint to the runtime.
    pub received: DirectionTraffic,
}

impl TrafficSnapshot {
    pub fn saturating_sub(self, earlier: Self) -> Self {
        Self {
            sent: self.sent.saturating_sub(earlier.sent),
            received: self.received.saturating_sub(earlier.received),
        }
    }
}

#[derive(Clone, Default)]
pub struct TrafficMeter {
    shared: Arc<Mutex<TrafficSnapshot>>,
}

impl TrafficMeter {
    pub fn snapshot(&self) -> TrafficSnapshot {
        *lock_unpoisoned(&self.shared)
    }
}

/// Endpoint decorator used by production transports and deterministic labs.
///
/// Only successful sends and delivered receives count toward wire budgets.
/// Retries therefore count exactly as they would on a real socket.  Datagram
/// classification uses the versioned AFC runtime envelope and leaves malformed
/// packets in the explicit `unclassified` bucket.
pub struct MeteredEndpoint<E> {
    endpoint: E,
    meter: TrafficMeter,
}

impl<E> MeteredEndpoint<E> {
    pub fn new(endpoint: E) -> (Self, TrafficMeter) {
        let meter = TrafficMeter::default();
        (
            Self {
                endpoint,
                meter: meter.clone(),
            },
            meter,
        )
    }

    pub const fn inner(&self) -> &E {
        &self.endpoint
    }

    pub fn inner_mut(&mut self) -> &mut E {
        &mut self.endpoint
    }
}

impl<E: NonBlockingDatagramEndpoint> NonBlockingDatagramEndpoint for MeteredEndpoint<E> {
    fn try_send(&mut self, datagram: AfcDatagram) -> SendOutcome {
        let retained = datagram.clone();
        let outcome = self.endpoint.try_send(datagram);
        if matches!(outcome, SendOutcome::Sent) {
            lock_unpoisoned(&self.meter.shared).sent.record(&retained);
        }
        outcome
    }

    fn try_receive(&mut self) -> ReceiveOutcome {
        let outcome = self.endpoint.try_receive();
        if let ReceiveOutcome::Received(datagram) = &outcome {
            lock_unpoisoned(&self.meter.shared)
                .received
                .record(datagram);
        }
        outcome
    }
}

fn runtime_channel(datagram: &AfcDatagram) -> Option<AfcChannel> {
    let bytes = datagram.as_slice();
    (bytes.len() > RUNTIME_CHANNEL_OFFSET && bytes.get(..4) == Some(&RUNTIME_MAGIC))
        .then(|| AfcChannel::try_from(bytes[RUNTIME_CHANNEL_OFFSET]).ok())
        .flatten()
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Exact average byte rate for a canonical-tick interval, rounded upward.
pub const fn average_bytes_per_second(bytes: u64, ticks: u64) -> u64 {
    if bytes == 0 || ticks == 0 {
        return 0;
    }
    bytes
        .saturating_mul(NETWORK_LAB_HZ)
        .saturating_add(ticks - 1)
        / ticks
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PeerTrafficSnapshot {
    pub peer_id: PeerId,
    pub client: TrafficSnapshot,
    pub authority: TrafficSnapshot,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PeerRuntimeMetrics {
    pub peer_id: PeerId,
    pub client: RuntimeMetrics,
    pub authority: RuntimeMetrics,
}

impl PeerTrafficSnapshot {
    pub fn saturating_sub(self, earlier: Self) -> Self {
        debug_assert_eq!(self.peer_id, earlier.peer_id);
        Self {
            peer_id: self.peer_id,
            client: self.client.saturating_sub(earlier.client),
            authority: self.authority.saturating_sub(earlier.authority),
        }
    }

    pub fn upstream_bytes(self) -> u64 {
        self.client.sent.total().bytes
    }

    pub fn downstream_bytes(self) -> u64 {
        self.authority.sent.total().bytes
    }
}

pub struct PeerEndpointPair<E> {
    pub peer_id: PeerId,
    pub client: E,
    pub authority: E,
}

pub struct TaggedRuntimeEvent {
    pub peer_id: PeerId,
    pub event: RuntimeEvent,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MultiPeerPumpReport {
    pub tick: SimTick,
    pub client_received_datagrams: u16,
    pub client_sent_datagrams: u16,
    pub authority_received_datagrams: u16,
    pub authority_sent_datagrams: u16,
    pub queued_events: u16,
}

impl MultiPeerPumpReport {
    fn add_client(&mut self, report: PumpReport) {
        self.client_received_datagrams = self
            .client_received_datagrams
            .saturating_add(report.received_datagrams);
        self.client_sent_datagrams = self
            .client_sent_datagrams
            .saturating_add(report.sent_datagrams);
        self.queued_events = self.queued_events.saturating_add(report.queued_events);
    }

    fn add_authority(&mut self, report: PumpReport) {
        self.authority_received_datagrams = self
            .authority_received_datagrams
            .saturating_add(report.received_datagrams);
        self.authority_sent_datagrams = self
            .authority_sent_datagrams
            .saturating_add(report.sent_datagrams);
        self.queued_events = self.queued_events.saturating_add(report.queued_events);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MultiPeerRuntimeError {
    Protocol(ProtocolValidationError),
    Session(SessionError),
    RuntimeConfig(RuntimeConfigError),
    RuntimeQueue(RuntimeQueueError),
    EmptyPeerSet,
    PeerCapacityExceeded { requested: usize, maximum: usize },
    DuplicatePeer(PeerId),
    MissingManifestPeer(PeerId),
    UnexpectedPeer(PeerId),
    UnknownPeer(PeerId),
    CountdownAlreadyBroadcast,
    PeersNotReady,
}

impl fmt::Display for MultiPeerRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "multi-peer runtime coordination failed: {self:?}"
        )
    }
}

impl std::error::Error for MultiPeerRuntimeError {}

impl From<ProtocolValidationError> for MultiPeerRuntimeError {
    fn from(error: ProtocolValidationError) -> Self {
        Self::Protocol(error)
    }
}

impl From<SessionError> for MultiPeerRuntimeError {
    fn from(error: SessionError) -> Self {
        Self::Session(error)
    }
}

impl From<RuntimeConfigError> for MultiPeerRuntimeError {
    fn from(error: RuntimeConfigError) -> Self {
        Self::RuntimeConfig(error)
    }
}

impl From<RuntimeQueueError> for MultiPeerRuntimeError {
    fn from(error: RuntimeQueueError) -> Self {
        Self::RuntimeQueue(error)
    }
}

struct PeerRuntimePair<E: NonBlockingDatagramEndpoint> {
    peer_id: PeerId,
    client: NetworkRuntime<MeteredEndpoint<E>>,
    authority: NetworkRuntime<MeteredEndpoint<E>>,
    client_meter: TrafficMeter,
    authority_meter: TrafficMeter,
}

/// Bounded fan-out for all remote peers in one match.
///
/// A copied [`AuthoritySessionGate`] lives in every connection runtime because
/// session messages are connection-local.  [`all_ready`](Self::all_ready) joins
/// those local views, and [`broadcast_countdown`](Self::broadcast_countdown)
/// performs the one global consensus action.  This avoids a false dependency on
/// any individual gate's `all_ready` bit in a multi-peer match.
pub struct MultiPeerRuntimeCoordinator<E: NonBlockingDatagramEndpoint> {
    manifest: MatchManifest,
    peers: Vec<PeerRuntimePair<E>>,
    countdown_start_tick: Option<SimTick>,
}

impl<E: NonBlockingDatagramEndpoint> MultiPeerRuntimeCoordinator<E> {
    pub fn new(
        manifest: MatchManifest,
        endpoint_pairs: Vec<PeerEndpointPair<E>>,
        config: RuntimeConfig,
    ) -> Result<Self, MultiPeerRuntimeError> {
        manifest.validate_for_start(SimTick::ZERO)?;
        config.validate()?;
        if endpoint_pairs.is_empty() {
            return Err(MultiPeerRuntimeError::EmptyPeerSet);
        }
        if endpoint_pairs.len() > MAX_NETWORK_PEERS {
            return Err(MultiPeerRuntimeError::PeerCapacityExceeded {
                requested: endpoint_pairs.len(),
                maximum: MAX_NETWORK_PEERS,
            });
        }

        let mut expected = [None; MAX_NETWORK_PEERS];
        let mut expected_len = 0;
        for assignment in manifest.ownership.as_slice() {
            let crate::network_protocol::SeatOwner::Peer(peer_id) = assignment.owner else {
                continue;
            };
            if expected[..expected_len].contains(&Some(peer_id)) {
                continue;
            }
            expected[expected_len] = Some(peer_id);
            expected_len += 1;
        }
        if expected_len == 0 {
            return Err(MultiPeerRuntimeError::EmptyPeerSet);
        }

        for (index, pair) in endpoint_pairs.iter().enumerate() {
            pair.peer_id.validate()?;
            if endpoint_pairs[..index]
                .iter()
                .any(|prior| prior.peer_id == pair.peer_id)
            {
                return Err(MultiPeerRuntimeError::DuplicatePeer(pair.peer_id));
            }
            if !expected[..expected_len].contains(&Some(pair.peer_id)) {
                return Err(MultiPeerRuntimeError::UnexpectedPeer(pair.peer_id));
            }
        }
        for peer_id in expected[..expected_len].iter().flatten() {
            if !endpoint_pairs.iter().any(|pair| pair.peer_id == *peer_id) {
                return Err(MultiPeerRuntimeError::MissingManifestPeer(*peer_id));
            }
        }

        let mut peers = Vec::with_capacity(endpoint_pairs.len());
        for pair in endpoint_pairs {
            let mut client_session = ClientSession::new(
                manifest.compatibility,
                SessionTimeouts::default(),
                SimTick::ZERO,
            )?;
            client_session.enter_lobby(SimTick::ZERO)?;
            client_session.start_connecting(SimTick::ZERO)?;
            client_session.transport_connected(SimTick::ZERO)?;
            client_session.authentication_succeeded(pair.peer_id, SimTick::ZERO)?;

            let (client_endpoint, client_meter) = MeteredEndpoint::new(pair.client);
            let (authority_endpoint, authority_meter) = MeteredEndpoint::new(pair.authority);
            let mut client = NetworkRuntime::new_client(
                client_endpoint,
                manifest.compatibility,
                client_session,
                config,
            )?;
            let mut authority = NetworkRuntime::new_authority(
                authority_endpoint,
                manifest.compatibility,
                AuthoritySessionGate::new(manifest)?,
                pair.peer_id,
                config,
            )?;
            client.queue_message(WireMessage::Handshake(Handshake {
                compatibility: manifest.compatibility,
            }))?;
            authority.queue_start_message(StartMessage::Manifest(manifest))?;
            peers.push(PeerRuntimePair {
                peer_id: pair.peer_id,
                client,
                authority,
                client_meter,
                authority_meter,
            });
        }

        Ok(Self {
            manifest,
            peers,
            countdown_start_tick: None,
        })
    }

    pub const fn manifest(&self) -> &MatchManifest {
        &self.manifest
    }

    pub fn peer_count(&self) -> usize {
        self.peers.len()
    }

    pub fn peer_ids(&self) -> impl ExactSizeIterator<Item = PeerId> + '_ {
        self.peers.iter().map(|peer| peer.peer_id)
    }

    pub fn peer_seat_count(&self, peer_id: PeerId) -> Result<usize, MultiPeerRuntimeError> {
        self.peer_index(peer_id)?;
        Ok(self
            .manifest
            .ownership
            .as_slice()
            .iter()
            .filter(|assignment| {
                assignment.owner == crate::network_protocol::SeatOwner::Peer(peer_id)
            })
            .count())
    }

    pub fn client_phase(&self, peer_id: PeerId) -> Result<ConnectionPhase, MultiPeerRuntimeError> {
        let peer = &self.peers[self.peer_index(peer_id)?];
        Ok(peer
            .client
            .client_session()
            .expect("coordinator client runtime owns a session")
            .phase())
    }

    pub fn client_runtime_mut(
        &mut self,
        peer_id: PeerId,
    ) -> Result<&mut NetworkRuntime<MeteredEndpoint<E>>, MultiPeerRuntimeError> {
        let index = self.peer_index(peer_id)?;
        Ok(&mut self.peers[index].client)
    }

    pub fn authority_runtime_mut(
        &mut self,
        peer_id: PeerId,
    ) -> Result<&mut NetworkRuntime<MeteredEndpoint<E>>, MultiPeerRuntimeError> {
        let index = self.peer_index(peer_id)?;
        Ok(&mut self.peers[index].authority)
    }

    pub fn queue_client(
        &mut self,
        peer_id: PeerId,
        message: WireMessage,
    ) -> Result<QueueDisposition, MultiPeerRuntimeError> {
        Ok(self.client_runtime_mut(peer_id)?.queue_message(message)?)
    }

    pub fn queue_authority(
        &mut self,
        peer_id: PeerId,
        message: WireMessage,
    ) -> Result<QueueDisposition, MultiPeerRuntimeError> {
        Ok(self
            .authority_runtime_mut(peer_id)?
            .queue_message(message)?)
    }

    pub fn broadcast_authority(
        &mut self,
        message: WireMessage,
    ) -> Result<(), MultiPeerRuntimeError> {
        for peer in &mut self.peers {
            peer.authority.queue_message(message.clone())?;
        }
        Ok(())
    }

    pub fn pump_clients(&mut self, tick: SimTick) -> MultiPeerPumpReport {
        let mut aggregate = MultiPeerPumpReport {
            tick,
            ..MultiPeerPumpReport::default()
        };
        for peer in &mut self.peers {
            aggregate.add_client(peer.client.pump(tick));
        }
        aggregate
    }

    pub fn pump_authorities(&mut self, tick: SimTick) -> MultiPeerPumpReport {
        let mut aggregate = MultiPeerPumpReport {
            tick,
            ..MultiPeerPumpReport::default()
        };
        for peer in &mut self.peers {
            aggregate.add_authority(peer.authority.pump(tick));
        }
        aggregate
    }

    pub fn pump_all(&mut self, tick: SimTick) -> MultiPeerPumpReport {
        let mut report = self.pump_clients(tick);
        let authority = self.pump_authorities(tick);
        report.authority_received_datagrams = authority.authority_received_datagrams;
        report.authority_sent_datagrams = authority.authority_sent_datagrams;
        report.queued_events = report.queued_events.saturating_add(authority.queued_events);
        report
    }

    pub fn drain_client_events(&mut self, mut consume: impl FnMut(TaggedRuntimeEvent)) {
        for peer in &mut self.peers {
            while let Some(event) = peer.client.try_next_event() {
                consume(TaggedRuntimeEvent {
                    peer_id: peer.peer_id,
                    event,
                });
            }
        }
    }

    pub fn drain_authority_events(&mut self, mut consume: impl FnMut(TaggedRuntimeEvent)) {
        for peer in &mut self.peers {
            while let Some(event) = peer.authority.try_next_event() {
                consume(TaggedRuntimeEvent {
                    peer_id: peer.peer_id,
                    event,
                });
            }
        }
    }

    /// True only when every connection-local gate has completed readiness for
    /// the peer represented by that connection.
    pub fn all_ready(&self) -> bool {
        self.peers.iter().all(|peer| {
            peer.authority
                .authority_gate()
                .and_then(|gate| gate.peer(peer.peer_id))
                .is_some_and(|readiness| readiness.ready)
        })
    }

    pub fn broadcast_countdown(&mut self, now: SimTick) -> Result<(), MultiPeerRuntimeError> {
        if self.countdown_start_tick.is_some() {
            return Err(MultiPeerRuntimeError::CountdownAlreadyBroadcast);
        }
        if !self.all_ready() {
            return Err(MultiPeerRuntimeError::PeersNotReady);
        }
        let after_lead = SimTick(
            now.0
                .checked_add(u64::from(DEFAULT_COUNTDOWN_LEAD_TICKS))
                .ok_or(SessionError::TimelineExhausted)?,
        );
        let start_tick = self.manifest.agreed_start_tick.max(after_lead);
        let countdown = StartMessage::Countdown {
            match_id: self.manifest.match_id,
            start_tick,
        };
        for peer in &mut self.peers {
            peer.authority.queue_start_message(countdown)?;
        }
        self.countdown_start_tick = Some(start_tick);
        Ok(())
    }

    pub const fn countdown_was_broadcast(&self) -> bool {
        self.countdown_start_tick.is_some()
    }

    pub const fn countdown_start_tick(&self) -> Option<SimTick> {
        self.countdown_start_tick
    }

    pub fn traffic(&self) -> Vec<PeerTrafficSnapshot> {
        self.peers
            .iter()
            .map(|peer| PeerTrafficSnapshot {
                peer_id: peer.peer_id,
                client: peer.client_meter.snapshot(),
                authority: peer.authority_meter.snapshot(),
            })
            .collect()
    }

    pub fn runtime_metrics(&self) -> Vec<PeerRuntimeMetrics> {
        self.peers
            .iter()
            .map(|peer| PeerRuntimeMetrics {
                peer_id: peer.peer_id,
                client: *peer.client.metrics(),
                authority: *peer.authority.metrics(),
            })
            .collect()
    }

    fn peer_index(&self, peer_id: PeerId) -> Result<usize, MultiPeerRuntimeError> {
        self.peers
            .iter()
            .position(|peer| peer.peer_id == peer_id)
            .ok_or(MultiPeerRuntimeError::UnknownPeer(peer_id))
    }
}
