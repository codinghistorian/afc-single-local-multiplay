//! Transport-independent, bounded AFC datagram I/O and deterministic fault injection.
//!
//! The production code in this module is intentionally `std`-only. It does not
//! know about Bevy, Lightyear, Steam, the AFC wire codec, or wall-clock time.
//! Transport adapters exchange opaque datagrams whose maximum size is fixed at
//! 1,200 bytes. The fault layer advances only when its caller supplies a network
//! tick, which makes every injected fault reproducible from a configuration and
//! seed.

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::fmt;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering as AtomicOrdering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Mutex, MutexGuard, TryLockError};

/// AFC's hard transport datagram ceiling. Wire headers and payload both fit here.
pub const MAX_AFC_DATAGRAM_BYTES: usize = 1_200;
pub const AFC_CHANNEL_COUNT: usize = 5;
pub const DEFAULT_IN_PROCESS_QUEUE_PACKETS: usize = 256;
pub const MAX_IN_PROCESS_QUEUE_PACKETS: usize = 4_096;
pub const DEFAULT_FAULT_QUEUE_PACKETS: usize = 512;
pub const MAX_FAULT_QUEUE_PACKETS: usize = 4_096;
pub const MAX_FAULT_DELAY_TICKS: u32 = 1_000_000;
pub const MAX_FAULT_BANDWIDTH_BURST_BYTES: u32 =
    (MAX_AFC_DATAGRAM_BYTES * MAX_FAULT_QUEUE_PACKETS) as u32;
pub const PROBABILITY_SCALE: u16 = 10_000;

/// Stable transport channel identifiers. These values are part of AFC's adapter
/// contract and intentionally match the wire codec's channel tags.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum AfcChannel {
    Control = 1,
    Input = 2,
    State = 3,
    Resync = 4,
    Result = 5,
}

impl AfcChannel {
    pub const ALL: [Self; AFC_CHANNEL_COUNT] = [
        Self::Control,
        Self::Input,
        Self::State,
        Self::Resync,
        Self::Result,
    ];

    pub const fn wire_id(self) -> u8 {
        self as u8
    }

    pub const fn metadata(self) -> ChannelMetadata {
        match self {
            Self::Control => AFC_CHANNELS[0],
            Self::Input => AFC_CHANNELS[1],
            Self::State => AFC_CHANNELS[2],
            Self::Resync => AFC_CHANNELS[3],
            Self::Result => AFC_CHANNELS[4],
        }
    }
}

impl TryFrom<u8> for AfcChannel {
    type Error = UnknownChannel;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Control),
            2 => Ok(Self::Input),
            3 => Ok(Self::State),
            4 => Ok(Self::Resync),
            5 => Ok(Self::Result),
            value => Err(UnknownChannel(value)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnknownChannel(pub u8);

impl fmt::Display for UnknownChannel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "unknown AFC channel {}", self.0)
    }
}

impl std::error::Error for UnknownChannel {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliverySemantics {
    OrderedReliable,
    SequencedUnreliable,
    UnorderedReliable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrafficDirection {
    Bidirectional,
    AuthorityToClient,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndpointRole {
    Client,
    Authority,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChannelMetadata {
    pub channel: AfcChannel,
    pub delivery: DeliverySemantics,
    pub direction: TrafficDirection,
}

impl ChannelMetadata {
    pub const fn permits_sender(self, role: EndpointRole) -> bool {
        match self.direction {
            TrafficDirection::Bidirectional => true,
            TrafficDirection::AuthorityToClient => matches!(role, EndpointRole::Authority),
        }
    }
}

/// The complete and exact AFC channel registration contract.
pub const AFC_CHANNELS: [ChannelMetadata; AFC_CHANNEL_COUNT] = [
    ChannelMetadata {
        channel: AfcChannel::Control,
        delivery: DeliverySemantics::OrderedReliable,
        direction: TrafficDirection::Bidirectional,
    },
    ChannelMetadata {
        channel: AfcChannel::Input,
        delivery: DeliverySemantics::SequencedUnreliable,
        direction: TrafficDirection::Bidirectional,
    },
    ChannelMetadata {
        channel: AfcChannel::State,
        delivery: DeliverySemantics::SequencedUnreliable,
        direction: TrafficDirection::AuthorityToClient,
    },
    ChannelMetadata {
        channel: AfcChannel::Resync,
        delivery: DeliverySemantics::UnorderedReliable,
        direction: TrafficDirection::AuthorityToClient,
    },
    ChannelMetadata {
        channel: AfcChannel::Result,
        delivery: DeliverySemantics::OrderedReliable,
        direction: TrafficDirection::AuthorityToClient,
    },
];

/// An opaque, fixed-capacity AFC transport datagram.
///
/// Construction rejects an over-limit slice before copying it. Unused bytes are
/// always zero, so keeping a datagram in a bounded queue has a fixed memory cost
/// and cannot hide attacker-controlled allocation.
#[derive(Clone, PartialEq, Eq)]
pub struct AfcDatagram {
    len: u16,
    bytes: [u8; MAX_AFC_DATAGRAM_BYTES],
}

impl AfcDatagram {
    pub fn try_from_slice(bytes: &[u8]) -> Result<Self, DatagramSizeError> {
        if bytes.len() > MAX_AFC_DATAGRAM_BYTES {
            return Err(DatagramSizeError {
                received: bytes.len(),
                maximum: MAX_AFC_DATAGRAM_BYTES,
            });
        }

        let mut datagram = Self::default();
        datagram.len = bytes.len() as u16;
        datagram.bytes[..bytes.len()].copy_from_slice(bytes);
        Ok(datagram)
    }

    pub const fn len(&self) -> usize {
        self.len as usize
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.len()]
    }
}

impl Default for AfcDatagram {
    fn default() -> Self {
        Self {
            len: 0,
            bytes: [0; MAX_AFC_DATAGRAM_BYTES],
        }
    }
}

impl fmt::Debug for AfcDatagram {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let shown = self.len().min(16);
        formatter
            .debug_struct("AfcDatagram")
            .field("len", &self.len())
            .field("prefix", &&self.as_slice()[..shown])
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DatagramSizeError {
    pub received: usize,
    pub maximum: usize,
}

impl fmt::Display for DatagramSizeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "AFC datagram is {} bytes; maximum is {}",
            self.received, self.maximum
        )
    }
}

impl std::error::Error for DatagramSizeError {}

/// A nonblocking send always returns ownership on failure.
#[derive(Debug, PartialEq, Eq)]
pub enum SendOutcome {
    Sent,
    /// A bounded userspace queue or OS send buffer could not accept the packet.
    Full(AfcDatagram),
    Disconnected(AfcDatagram),
    IoError {
        datagram: AfcDatagram,
        kind: io::ErrorKind,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub enum ReceiveOutcome {
    Received(AfcDatagram),
    Empty,
    Disconnected,
    /// UDP supplied at least this many bytes. The entire datagram was discarded.
    Oversized {
        observed_at_least: usize,
    },
    IoError(io::ErrorKind),
}

pub trait NonBlockingDatagramEndpoint {
    fn try_send(&mut self, datagram: AfcDatagram) -> SendOutcome;
    fn try_receive(&mut self) -> ReceiveOutcome;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QueueMetrics {
    pub capacity_packets: usize,
    pub depth_packets: usize,
    pub high_water_packets: usize,
    pub send_attempts: u64,
    pub sent_packets: u64,
    pub sent_bytes: u64,
    pub received_packets: u64,
    pub received_bytes: u64,
    pub full_send_attempts: u64,
    pub disconnected_send_attempts: u64,
    pub empty_receive_attempts: u64,
    pub disconnected_receive_attempts: u64,
    pub discarded_on_receiver_drop: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EndpointQueueMetrics {
    pub outbound: QueueMetrics,
    pub inbound: QueueMetrics,
}

struct QueueCounters {
    capacity: usize,
    depth: AtomicUsize,
    high_water: AtomicUsize,
    send_attempts: AtomicU64,
    sent_packets: AtomicU64,
    sent_bytes: AtomicU64,
    received_packets: AtomicU64,
    received_bytes: AtomicU64,
    full_send_attempts: AtomicU64,
    disconnected_send_attempts: AtomicU64,
    empty_receive_attempts: AtomicU64,
    disconnected_receive_attempts: AtomicU64,
    discarded_on_receiver_drop: AtomicU64,
    receiver_alive: AtomicBool,
}

impl QueueCounters {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            depth: AtomicUsize::new(0),
            high_water: AtomicUsize::new(0),
            send_attempts: AtomicU64::new(0),
            sent_packets: AtomicU64::new(0),
            sent_bytes: AtomicU64::new(0),
            received_packets: AtomicU64::new(0),
            received_bytes: AtomicU64::new(0),
            full_send_attempts: AtomicU64::new(0),
            disconnected_send_attempts: AtomicU64::new(0),
            empty_receive_attempts: AtomicU64::new(0),
            disconnected_receive_attempts: AtomicU64::new(0),
            discarded_on_receiver_drop: AtomicU64::new(0),
            receiver_alive: AtomicBool::new(true),
        }
    }

    fn reserve(&self) -> Option<usize> {
        let mut depth = self.depth.load(AtomicOrdering::Relaxed);
        loop {
            if depth >= self.capacity {
                return None;
            }
            match self.depth.compare_exchange_weak(
                depth,
                depth + 1,
                AtomicOrdering::AcqRel,
                AtomicOrdering::Relaxed,
            ) {
                Ok(_) => return Some(depth + 1),
                Err(actual) => depth = actual,
            }
        }
    }

    fn release(&self) {
        let _ = self
            .depth
            .fetch_update(AtomicOrdering::AcqRel, AtomicOrdering::Relaxed, |depth| {
                Some(depth.saturating_sub(1))
            });
    }

    fn record_high_water(&self, candidate: usize) {
        let mut high_water = self.high_water.load(AtomicOrdering::Relaxed);
        while candidate > high_water {
            match self.high_water.compare_exchange_weak(
                high_water,
                candidate,
                AtomicOrdering::Relaxed,
                AtomicOrdering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => high_water = actual,
            }
        }
    }

    fn snapshot(&self) -> QueueMetrics {
        QueueMetrics {
            capacity_packets: self.capacity,
            depth_packets: self.depth.load(AtomicOrdering::Relaxed),
            high_water_packets: self.high_water.load(AtomicOrdering::Relaxed),
            send_attempts: self.send_attempts.load(AtomicOrdering::Relaxed),
            sent_packets: self.sent_packets.load(AtomicOrdering::Relaxed),
            sent_bytes: self.sent_bytes.load(AtomicOrdering::Relaxed),
            received_packets: self.received_packets.load(AtomicOrdering::Relaxed),
            received_bytes: self.received_bytes.load(AtomicOrdering::Relaxed),
            full_send_attempts: self.full_send_attempts.load(AtomicOrdering::Relaxed),
            disconnected_send_attempts: self
                .disconnected_send_attempts
                .load(AtomicOrdering::Relaxed),
            empty_receive_attempts: self.empty_receive_attempts.load(AtomicOrdering::Relaxed),
            disconnected_receive_attempts: self
                .disconnected_receive_attempts
                .load(AtomicOrdering::Relaxed),
            discarded_on_receiver_drop: self
                .discarded_on_receiver_drop
                .load(AtomicOrdering::Relaxed),
        }
    }
}

fn atomic_saturating_add(counter: &AtomicU64, amount: u64) {
    let _ = counter.fetch_update(AtomicOrdering::Relaxed, AtomicOrdering::Relaxed, |value| {
        Some(value.saturating_add(amount))
    });
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InProcessConfigError {
    ZeroCapacity,
    CapacityExceeded { requested: usize, maximum: usize },
}

impl fmt::Display for InProcessConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid AFC in-process queue configuration: {self:?}"
        )
    }
}

impl std::error::Error for InProcessConfigError {}

/// One endpoint of a bounded, nonblocking, full-duplex in-process link.
///
/// Each direction has an independent queue and metrics object. `try_send` and
/// `try_receive` use the standard library's nonblocking channel operations; no
/// call waits for the peer.
pub struct InProcessEndpoint {
    sender: SyncSender<AfcDatagram>,
    receiver: Receiver<AfcDatagram>,
    outbound: Arc<QueueCounters>,
    inbound: Arc<QueueCounters>,
}

impl InProcessEndpoint {
    pub fn pair(capacity_packets: usize) -> Result<(Self, Self), InProcessConfigError> {
        if capacity_packets == 0 {
            return Err(InProcessConfigError::ZeroCapacity);
        }
        if capacity_packets > MAX_IN_PROCESS_QUEUE_PACKETS {
            return Err(InProcessConfigError::CapacityExceeded {
                requested: capacity_packets,
                maximum: MAX_IN_PROCESS_QUEUE_PACKETS,
            });
        }

        let (a_to_b_sender, a_to_b_receiver) = mpsc::sync_channel(capacity_packets);
        let (b_to_a_sender, b_to_a_receiver) = mpsc::sync_channel(capacity_packets);
        let a_to_b = Arc::new(QueueCounters::new(capacity_packets));
        let b_to_a = Arc::new(QueueCounters::new(capacity_packets));

        let endpoint_a = Self {
            sender: a_to_b_sender,
            receiver: b_to_a_receiver,
            outbound: Arc::clone(&a_to_b),
            inbound: Arc::clone(&b_to_a),
        };
        let endpoint_b = Self {
            sender: b_to_a_sender,
            receiver: a_to_b_receiver,
            outbound: b_to_a,
            inbound: a_to_b,
        };
        Ok((endpoint_a, endpoint_b))
    }

    pub fn metrics(&self) -> EndpointQueueMetrics {
        EndpointQueueMetrics {
            outbound: self.outbound.snapshot(),
            inbound: self.inbound.snapshot(),
        }
    }
}

impl NonBlockingDatagramEndpoint for InProcessEndpoint {
    fn try_send(&mut self, datagram: AfcDatagram) -> SendOutcome {
        atomic_saturating_add(&self.outbound.send_attempts, 1);
        // Checking the receiver liveness before the local capacity reservation
        // ensures a full queue whose receiver was dropped reports Disconnected,
        // rather than remaining permanently Full with stale depth metrics.
        if !self.outbound.receiver_alive.load(AtomicOrdering::Acquire) {
            atomic_saturating_add(&self.outbound.disconnected_send_attempts, 1);
            return SendOutcome::Disconnected(datagram);
        }
        let Some(reserved_depth) = self.outbound.reserve() else {
            atomic_saturating_add(&self.outbound.full_send_attempts, 1);
            return SendOutcome::Full(datagram);
        };
        let bytes = datagram.len() as u64;

        match self.sender.try_send(datagram) {
            Ok(()) => {
                self.outbound.record_high_water(reserved_depth);
                atomic_saturating_add(&self.outbound.sent_packets, 1);
                atomic_saturating_add(&self.outbound.sent_bytes, bytes);
                SendOutcome::Sent
            }
            Err(TrySendError::Full(datagram)) => {
                self.outbound.release();
                atomic_saturating_add(&self.outbound.full_send_attempts, 1);
                SendOutcome::Full(datagram)
            }
            Err(TrySendError::Disconnected(datagram)) => {
                self.outbound.release();
                atomic_saturating_add(&self.outbound.disconnected_send_attempts, 1);
                SendOutcome::Disconnected(datagram)
            }
        }
    }

    fn try_receive(&mut self) -> ReceiveOutcome {
        match self.receiver.try_recv() {
            Ok(datagram) => {
                self.inbound.release();
                atomic_saturating_add(&self.inbound.received_packets, 1);
                atomic_saturating_add(&self.inbound.received_bytes, datagram.len() as u64);
                ReceiveOutcome::Received(datagram)
            }
            Err(TryRecvError::Empty) => {
                atomic_saturating_add(&self.inbound.empty_receive_attempts, 1);
                ReceiveOutcome::Empty
            }
            Err(TryRecvError::Disconnected) => {
                atomic_saturating_add(&self.inbound.disconnected_receive_attempts, 1);
                ReceiveOutcome::Disconnected
            }
        }
    }
}

impl Drop for InProcessEndpoint {
    fn drop(&mut self) {
        self.inbound
            .receiver_alive
            .store(false, AtomicOrdering::Release);
        let discarded = self.inbound.depth.swap(0, AtomicOrdering::AcqRel);
        atomic_saturating_add(&self.inbound.discarded_on_receiver_drop, discarded as u64);
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UdpMetrics {
    pub send_attempts: u64,
    pub sent_datagrams: u64,
    pub sent_bytes: u64,
    pub received_datagrams: u64,
    pub received_bytes: u64,
    pub send_would_block: u64,
    pub receive_would_block: u64,
    pub disconnected_errors: u64,
    pub other_io_errors: u64,
    pub oversized_datagrams: u64,
}

/// A connected, nonblocking ordinary UDP endpoint for CI and the pre-Steam gate.
pub struct UdpEndpoint {
    socket: UdpSocket,
    metrics: UdpMetrics,
}

impl UdpEndpoint {
    pub fn bind_connected(local: SocketAddr, peer: SocketAddr) -> io::Result<Self> {
        let socket = UdpSocket::bind(local)?;
        socket.connect(peer)?;
        Self::from_connected_socket(socket)
    }

    pub fn from_connected_socket(socket: UdpSocket) -> io::Result<Self> {
        // Fail early if the caller passed an unconnected socket.
        let _ = socket.peer_addr()?;
        socket.set_nonblocking(true)?;
        Ok(Self {
            socket,
            metrics: UdpMetrics::default(),
        })
    }

    pub fn loopback_pair() -> io::Result<(Self, Self)> {
        let loopback = Ipv4Addr::LOCALHOST;
        let socket_a = UdpSocket::bind(SocketAddrV4::new(loopback, 0))?;
        let socket_b = UdpSocket::bind(SocketAddrV4::new(loopback, 0))?;
        socket_a.connect(socket_b.local_addr()?)?;
        socket_b.connect(socket_a.local_addr()?)?;
        Ok((
            Self::from_connected_socket(socket_a)?,
            Self::from_connected_socket(socket_b)?,
        ))
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.socket.peer_addr()
    }

    pub const fn metrics(&self) -> UdpMetrics {
        self.metrics
    }
}

impl NonBlockingDatagramEndpoint for UdpEndpoint {
    fn try_send(&mut self, datagram: AfcDatagram) -> SendOutcome {
        self.metrics.send_attempts = self.metrics.send_attempts.saturating_add(1);
        match self.socket.send(datagram.as_slice()) {
            Ok(sent) if sent == datagram.len() => {
                self.metrics.sent_datagrams = self.metrics.sent_datagrams.saturating_add(1);
                self.metrics.sent_bytes = self.metrics.sent_bytes.saturating_add(sent as u64);
                SendOutcome::Sent
            }
            Ok(_) => {
                self.metrics.other_io_errors = self.metrics.other_io_errors.saturating_add(1);
                SendOutcome::IoError {
                    datagram,
                    kind: io::ErrorKind::WriteZero,
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                self.metrics.send_would_block = self.metrics.send_would_block.saturating_add(1);
                SendOutcome::Full(datagram)
            }
            Err(error) if is_disconnected_io_error(error.kind()) => {
                self.metrics.disconnected_errors =
                    self.metrics.disconnected_errors.saturating_add(1);
                SendOutcome::Disconnected(datagram)
            }
            Err(error) => {
                self.metrics.other_io_errors = self.metrics.other_io_errors.saturating_add(1);
                SendOutcome::IoError {
                    datagram,
                    kind: error.kind(),
                }
            }
        }
    }

    fn try_receive(&mut self) -> ReceiveOutcome {
        // The extra byte distinguishes a legal 1,200-byte packet from any packet
        // that the transport must discard. Larger UDP packets may be truncated to
        // this buffer, which is still enough to prove they crossed AFC's ceiling.
        let mut bytes = [0_u8; MAX_AFC_DATAGRAM_BYTES + 1];
        match self.socket.recv(&mut bytes) {
            Ok(received) if received <= MAX_AFC_DATAGRAM_BYTES => {
                let datagram = AfcDatagram::try_from_slice(&bytes[..received])
                    .expect("receive buffer enforces the AFC datagram ceiling");
                self.metrics.received_datagrams = self.metrics.received_datagrams.saturating_add(1);
                self.metrics.received_bytes =
                    self.metrics.received_bytes.saturating_add(received as u64);
                ReceiveOutcome::Received(datagram)
            }
            Ok(received) => {
                self.metrics.oversized_datagrams =
                    self.metrics.oversized_datagrams.saturating_add(1);
                ReceiveOutcome::Oversized {
                    observed_at_least: received,
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                self.metrics.receive_would_block =
                    self.metrics.receive_would_block.saturating_add(1);
                ReceiveOutcome::Empty
            }
            Err(error) if is_disconnected_io_error(error.kind()) => {
                self.metrics.disconnected_errors =
                    self.metrics.disconnected_errors.saturating_add(1);
                ReceiveOutcome::Disconnected
            }
            Err(error) => {
                self.metrics.other_io_errors = self.metrics.other_io_errors.saturating_add(1);
                ReceiveOutcome::IoError(error.kind())
            }
        }
    }
}

const fn is_disconnected_io_error(kind: io::ErrorKind) -> bool {
    matches!(
        kind,
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::NotConnected
            | io::ErrorKind::UnexpectedEof
    )
}

pub type NetworkTick = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisconnectWindow {
    /// The first disconnected tick.
    pub start_tick: NetworkTick,
    /// The first connected tick after the fault, or `None` for permanent loss.
    pub reconnect_tick: Option<NetworkTick>,
}

impl DisconnectWindow {
    pub const fn contains(self, tick: NetworkTick) -> bool {
        tick >= self.start_tick
            && match self.reconnect_tick {
                Some(reconnect_tick) => tick < reconnect_tick,
                None => true,
            }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FaultConfig {
    pub base_latency_ticks: u32,
    /// Uniform signed jitter in `-jitter_ticks..=jitter_ticks`.
    pub jitter_ticks: u32,
    pub loss_per_10k: u16,
    pub duplication_per_10k: u16,
    pub reorder_per_10k: u16,
    /// A selected reordered copy receives an additional `1..=max` ticks.
    pub max_reorder_extra_ticks: u32,
    /// Zero means unlimited. Otherwise tokens refill by this amount each tick.
    pub bandwidth_bytes_per_tick: u32,
    /// Token-bucket capacity. When bandwidth is limited this must be at least one
    /// maximum-size AFC datagram, so every legal packet can eventually pass.
    pub bandwidth_burst_bytes: u32,
    /// Values 0 and 1 disable burst gating. Larger values align delivery to the
    /// next global tick divisible by the interval.
    pub delivery_burst_interval_ticks: u32,
    pub queue_capacity_packets: usize,
    pub disconnect: Option<DisconnectWindow>,
}

impl Default for FaultConfig {
    fn default() -> Self {
        Self {
            base_latency_ticks: 0,
            jitter_ticks: 0,
            loss_per_10k: 0,
            duplication_per_10k: 0,
            reorder_per_10k: 0,
            max_reorder_extra_ticks: 0,
            bandwidth_bytes_per_tick: 0,
            bandwidth_burst_bytes: 0,
            delivery_burst_interval_ticks: 0,
            queue_capacity_packets: DEFAULT_FAULT_QUEUE_PACKETS,
            disconnect: None,
        }
    }
}

impl FaultConfig {
    pub fn validate(self) -> Result<Self, FaultConfigError> {
        if self.queue_capacity_packets == 0 {
            return Err(FaultConfigError::ZeroQueueCapacity);
        }
        if self.queue_capacity_packets > MAX_FAULT_QUEUE_PACKETS {
            return Err(FaultConfigError::QueueCapacityExceeded {
                requested: self.queue_capacity_packets,
                maximum: MAX_FAULT_QUEUE_PACKETS,
            });
        }
        if self.base_latency_ticks > MAX_FAULT_DELAY_TICKS
            || self.jitter_ticks > MAX_FAULT_DELAY_TICKS
            || self.max_reorder_extra_ticks > MAX_FAULT_DELAY_TICKS
        {
            return Err(FaultConfigError::DelayExceeded);
        }
        for (field, value) in [
            (FaultProbabilityField::Loss, self.loss_per_10k),
            (FaultProbabilityField::Duplication, self.duplication_per_10k),
            (FaultProbabilityField::Reorder, self.reorder_per_10k),
        ] {
            if value > PROBABILITY_SCALE {
                return Err(FaultConfigError::InvalidProbability { field, value });
            }
        }
        if self.reorder_per_10k > 0 && self.max_reorder_extra_ticks == 0 {
            return Err(FaultConfigError::MissingReorderDelay);
        }
        if self.bandwidth_burst_bytes > MAX_FAULT_BANDWIDTH_BURST_BYTES {
            return Err(FaultConfigError::BandwidthBurstExceeded);
        }
        if self.bandwidth_bytes_per_tick > 0
            && self.bandwidth_burst_bytes < MAX_AFC_DATAGRAM_BYTES as u32
        {
            return Err(FaultConfigError::BandwidthBurstTooSmall);
        }
        if let Some(window) = self.disconnect {
            if let Some(reconnect_tick) = window.reconnect_tick {
                if reconnect_tick <= window.start_tick {
                    return Err(FaultConfigError::InvalidDisconnectWindow);
                }
            }
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaultProbabilityField {
    Loss,
    Duplication,
    Reorder,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaultConfigError {
    ZeroQueueCapacity,
    QueueCapacityExceeded {
        requested: usize,
        maximum: usize,
    },
    DelayExceeded,
    InvalidProbability {
        field: FaultProbabilityField,
        value: u16,
    },
    MissingReorderDelay,
    BandwidthBurstTooSmall,
    BandwidthBurstExceeded,
    InvalidDisconnectWindow,
}

impl fmt::Display for FaultConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid deterministic fault configuration: {self:?}"
        )
    }
}

impl std::error::Error for FaultConfigError {}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FaultMetrics {
    pub injection_attempts: u64,
    pub injected_bytes: u64,
    pub accepted_copies: u64,
    pub accepted_bytes: u64,
    pub dropped_by_loss: u64,
    pub duplicate_copies: u64,
    pub reordered_copies: u64,
    pub queue_full_events: u64,
    pub disconnected_send_attempts: u64,
    pub disconnected_receive_attempts: u64,
    pub purged_on_disconnect: u64,
    pub delivered_datagrams: u64,
    pub delivered_bytes: u64,
    pub pending_datagrams: usize,
    pub pending_high_water: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub enum FaultSendOutcome {
    Accepted { scheduled_copies: u8 },
    DroppedByLoss,
    Full(AfcDatagram),
    Disconnected(AfcDatagram),
}

#[derive(Debug, PartialEq, Eq)]
pub enum FaultReceiveOutcome {
    Received(AfcDatagram),
    Empty,
    Disconnected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TickWentBackwards {
    pub current: NetworkTick,
    pub requested: NetworkTick,
}

impl fmt::Display for TickWentBackwards {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "network fault tick moved backwards from {} to {}",
            self.current, self.requested
        )
    }
}

impl std::error::Error for TickWentBackwards {}

/// Configuration for both directions of one deterministic in-memory network.
///
/// Directional configuration is intentional: consumer upload and download paths
/// commonly have different delay, loss, and bandwidth characteristics. Seeds are
/// recorded independently so adding traffic in one direction cannot perturb the
/// other direction's fault decisions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FaultLabConfig {
    pub a_to_b: FaultConfig,
    pub b_to_a: FaultConfig,
    pub a_to_b_seed: u64,
    pub b_to_a_seed: u64,
}

impl FaultLabConfig {
    pub const fn new(
        a_to_b: FaultConfig,
        b_to_a: FaultConfig,
        a_to_b_seed: u64,
        b_to_a_seed: u64,
    ) -> Self {
        Self {
            a_to_b,
            b_to_a,
            a_to_b_seed,
            b_to_a_seed,
        }
    }

    /// Builds a symmetric link while keeping the two RNG streams isolated.
    pub const fn symmetric(config: FaultConfig, seed: u64) -> Self {
        Self::new(
            config,
            config,
            seed ^ 0xAFC0_A2B0_D15C_0001,
            seed ^ 0xAFC0_B2A0_D15C_0002,
        )
    }

    /// The architecture's `NetTypical4` link at a 60 Hz network clock: 100 ms
    /// base RTT, nearest-integer 20 ms jitter (one tick), and 1% loss.
    pub const fn net_typical_60hz(seed: u64) -> Self {
        Self::symmetric(fault_scenario_direction(3, 1, 100), seed)
    }

    /// The architecture's `NetDegraded4` link at a 60 Hz network clock: 150 ms
    /// base RTT (four ticks A-to-B plus five ticks B-to-A), nearest-integer 30 ms
    /// jitter (two ticks), 3% loss, and low 1% duplicate/reorder injection.
    pub const fn net_degraded_60hz(seed: u64) -> Self {
        Self::new(
            fault_degraded_direction(4),
            fault_degraded_direction(5),
            seed ^ 0xAFC0_A2B0_D15C_0001,
            seed ^ 0xAFC0_B2A0_D15C_0002,
        )
    }
}

const fn fault_scenario_direction(
    base_latency_ticks: u32,
    jitter_ticks: u32,
    loss_per_10k: u16,
) -> FaultConfig {
    FaultConfig {
        base_latency_ticks,
        jitter_ticks,
        loss_per_10k,
        duplication_per_10k: 0,
        reorder_per_10k: 0,
        max_reorder_extra_ticks: 0,
        bandwidth_bytes_per_tick: 0,
        bandwidth_burst_bytes: 0,
        delivery_burst_interval_ticks: 0,
        queue_capacity_packets: DEFAULT_FAULT_QUEUE_PACKETS,
        disconnect: None,
    }
}

const fn fault_degraded_direction(base_latency_ticks: u32) -> FaultConfig {
    FaultConfig {
        duplication_per_10k: 100,
        reorder_per_10k: 100,
        max_reorder_extra_ticks: 1,
        ..fault_scenario_direction(base_latency_ticks, 2, 300)
    }
}

impl Default for FaultLabConfig {
    fn default() -> Self {
        Self::symmetric(FaultConfig::default(), 0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaultLabDirection {
    AToB,
    BToA,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FaultLabConfigError {
    pub direction: FaultLabDirection,
    pub source: FaultConfigError,
}

impl fmt::Display for FaultLabConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid {:?} deterministic fault link: {}",
            self.direction, self.source
        )
    }
}

impl std::error::Error for FaultLabConfigError {}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FaultLabEndpointMetrics {
    /// Sends rejected because the destination endpoint was dropped, independent
    /// of a configured disconnect window.
    pub peer_dropped_send_attempts: u64,
    /// Empty receives reported as disconnected after the sender endpoint dropped.
    pub peer_dropped_receive_attempts: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FaultLabMetrics {
    pub current_tick: NetworkTick,
    pub a_to_b: FaultMetrics,
    pub b_to_a: FaultMetrics,
    pub endpoint_a: FaultLabEndpointMetrics,
    pub endpoint_b: FaultLabEndpointMetrics,
    pub endpoint_a_alive: bool,
    pub endpoint_b_alive: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FaultLabSide {
    A,
    B,
}

impl FaultLabSide {
    const fn index(self) -> usize {
        match self {
            Self::A => 0,
            Self::B => 1,
        }
    }

    const fn peer_index(self) -> usize {
        1 - self.index()
    }
}

struct FaultLabState {
    current_tick: NetworkTick,
    a_to_b: DeterministicFaultLayer,
    b_to_a: DeterministicFaultLayer,
    endpoint_alive: [bool; 2],
    endpoint_metrics: [FaultLabEndpointMetrics; 2],
}

impl FaultLabState {
    fn outbound(&mut self, side: FaultLabSide) -> &mut DeterministicFaultLayer {
        match side {
            FaultLabSide::A => &mut self.a_to_b,
            FaultLabSide::B => &mut self.b_to_a,
        }
    }

    fn inbound(&mut self, side: FaultLabSide) -> &mut DeterministicFaultLayer {
        match side {
            FaultLabSide::A => &mut self.b_to_a,
            FaultLabSide::B => &mut self.a_to_b,
        }
    }
}

/// Clock and metrics handle for a pair of [`FaultLabEndpoint`]s.
///
/// The controller remains outside runtimes that own the endpoints. A test advances
/// this clock once per canonical network tick, then pumps any number of protocol
/// runtimes. No endpoint operation implicitly advances time.
#[derive(Clone)]
pub struct DeterministicNetworkLab {
    shared: Arc<Mutex<FaultLabState>>,
}

impl DeterministicNetworkLab {
    pub fn pair(
        config: FaultLabConfig,
    ) -> Result<(Self, FaultLabEndpoint, FaultLabEndpoint), FaultLabConfigError> {
        let a_to_b =
            DeterministicFaultLayer::new(config.a_to_b, config.a_to_b_seed).map_err(|source| {
                FaultLabConfigError {
                    direction: FaultLabDirection::AToB,
                    source,
                }
            })?;
        let b_to_a =
            DeterministicFaultLayer::new(config.b_to_a, config.b_to_a_seed).map_err(|source| {
                FaultLabConfigError {
                    direction: FaultLabDirection::BToA,
                    source,
                }
            })?;
        let shared = Arc::new(Mutex::new(FaultLabState {
            current_tick: 0,
            a_to_b,
            b_to_a,
            endpoint_alive: [true, true],
            endpoint_metrics: [FaultLabEndpointMetrics::default(); 2],
        }));
        Ok((
            Self {
                shared: Arc::clone(&shared),
            },
            FaultLabEndpoint {
                side: FaultLabSide::A,
                shared: Arc::clone(&shared),
            },
            FaultLabEndpoint {
                side: FaultLabSide::B,
                shared,
            },
        ))
    }

    pub fn advance_to(&self, tick: NetworkTick) -> Result<(), TickWentBackwards> {
        let mut state = lock_unpoisoned(&self.shared);
        if tick < state.current_tick {
            return Err(TickWentBackwards {
                current: state.current_tick,
                requested: tick,
            });
        }
        // Both layers are private to this controller and always share a clock.
        state
            .a_to_b
            .advance_to(tick)
            .expect("fault-lab direction clocks remain synchronized");
        state
            .b_to_a
            .advance_to(tick)
            .expect("fault-lab direction clocks remain synchronized");
        state.current_tick = tick;
        Ok(())
    }

    pub fn metrics(&self) -> FaultLabMetrics {
        let state = lock_unpoisoned(&self.shared);
        FaultLabMetrics {
            current_tick: state.current_tick,
            a_to_b: state.a_to_b.metrics(),
            b_to_a: state.b_to_a.metrics(),
            endpoint_a: state.endpoint_metrics[0],
            endpoint_b: state.endpoint_metrics[1],
            endpoint_a_alive: state.endpoint_alive[0],
            endpoint_b_alive: state.endpoint_alive[1],
        }
    }
}

/// One side of a deterministic, bounded, full-duplex fault laboratory.
///
/// Loss is reported to callers as a successful send, matching a real datagram
/// transport. Queue pressure and connection loss remain explicit so protocol
/// backpressure and reconnect paths can be exercised through the normal
/// [`NonBlockingDatagramEndpoint`] contract.
pub struct FaultLabEndpoint {
    side: FaultLabSide,
    shared: Arc<Mutex<FaultLabState>>,
}

impl NonBlockingDatagramEndpoint for FaultLabEndpoint {
    fn try_send(&mut self, datagram: AfcDatagram) -> SendOutcome {
        let Some(mut state) = try_lock_unpoisoned(&self.shared) else {
            // The controller or peer is advancing the laboratory. Surface normal
            // nonblocking backpressure and preserve ownership for a later retry.
            return SendOutcome::Full(datagram);
        };
        if !state.endpoint_alive[self.side.peer_index()] {
            let metrics = &mut state.endpoint_metrics[self.side.index()];
            metrics.peer_dropped_send_attempts =
                metrics.peer_dropped_send_attempts.saturating_add(1);
            return SendOutcome::Disconnected(datagram);
        }
        match state.outbound(self.side).enqueue(datagram) {
            FaultSendOutcome::Accepted { .. } | FaultSendOutcome::DroppedByLoss => {
                SendOutcome::Sent
            }
            FaultSendOutcome::Full(datagram) => SendOutcome::Full(datagram),
            FaultSendOutcome::Disconnected(datagram) => SendOutcome::Disconnected(datagram),
        }
    }

    fn try_receive(&mut self) -> ReceiveOutcome {
        let Some(mut state) = try_lock_unpoisoned(&self.shared) else {
            return ReceiveOutcome::Empty;
        };
        match state.inbound(self.side).try_receive() {
            FaultReceiveOutcome::Received(datagram) => ReceiveOutcome::Received(datagram),
            FaultReceiveOutcome::Disconnected => ReceiveOutcome::Disconnected,
            FaultReceiveOutcome::Empty if state.endpoint_alive[self.side.peer_index()] => {
                ReceiveOutcome::Empty
            }
            FaultReceiveOutcome::Empty => {
                let metrics = &mut state.endpoint_metrics[self.side.index()];
                metrics.peer_dropped_receive_attempts =
                    metrics.peer_dropped_receive_attempts.saturating_add(1);
                ReceiveOutcome::Disconnected
            }
        }
    }
}

impl Drop for FaultLabEndpoint {
    fn drop(&mut self) {
        lock_unpoisoned(&self.shared).endpoint_alive[self.side.index()] = false;
    }
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn try_lock_unpoisoned<T>(mutex: &Mutex<T>) -> Option<MutexGuard<'_, T>> {
    match mutex.try_lock() {
        Ok(guard) => Some(guard),
        Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
        Err(TryLockError::WouldBlock) => None,
    }
}

/// A deterministic one-way network fault layer.
///
/// Callers should place one instance on each traffic direction, with independent
/// recorded seeds. `advance_to` is the only source of time. Enqueueing, polling,
/// and repeatedly polling at one tick never reads or advances a wall clock.
pub struct DeterministicFaultLayer {
    config: FaultConfig,
    seed: u64,
    rng: SplitMix64,
    now: NetworkTick,
    next_ordinal: u64,
    scheduled: BinaryHeap<ScheduledDatagram>,
    bandwidth_tokens: u64,
    disconnect_purged: bool,
    metrics: FaultMetrics,
}

impl DeterministicFaultLayer {
    pub fn new(config: FaultConfig, seed: u64) -> Result<Self, FaultConfigError> {
        let config = config.validate()?;
        let bandwidth_tokens = if config.bandwidth_bytes_per_tick == 0 {
            u64::MAX
        } else {
            u64::from(config.bandwidth_burst_bytes)
        };
        let disconnect_purged = config
            .disconnect
            .map(|window| window.start_tick == 0)
            .unwrap_or(false);
        Ok(Self {
            config,
            seed,
            rng: SplitMix64::new(seed),
            now: 0,
            next_ordinal: 0,
            scheduled: BinaryHeap::with_capacity(config.queue_capacity_packets),
            bandwidth_tokens,
            disconnect_purged,
            metrics: FaultMetrics::default(),
        })
    }

    pub const fn seed(&self) -> u64 {
        self.seed
    }

    pub const fn current_tick(&self) -> NetworkTick {
        self.now
    }

    pub const fn config(&self) -> FaultConfig {
        self.config
    }

    pub fn is_disconnected(&self) -> bool {
        self.config
            .disconnect
            .map(|window| window.contains(self.now))
            .unwrap_or(false)
    }

    pub fn metrics(&self) -> FaultMetrics {
        let mut metrics = self.metrics;
        metrics.pending_datagrams = self.scheduled.len();
        metrics
    }

    pub fn advance_to(&mut self, tick: NetworkTick) -> Result<(), TickWentBackwards> {
        if tick < self.now {
            return Err(TickWentBackwards {
                current: self.now,
                requested: tick,
            });
        }

        if let Some(window) = self.config.disconnect {
            if !self.disconnect_purged && self.now < window.start_tick && tick >= window.start_tick
            {
                let purged = self.scheduled.len() as u64;
                self.scheduled.clear();
                self.metrics.purged_on_disconnect =
                    self.metrics.purged_on_disconnect.saturating_add(purged);
                self.disconnect_purged = true;
            }
        }

        let elapsed = tick - self.now;
        if self.config.bandwidth_bytes_per_tick > 0 {
            let refill = elapsed.saturating_mul(u64::from(self.config.bandwidth_bytes_per_tick));
            self.bandwidth_tokens = self
                .bandwidth_tokens
                .saturating_add(refill)
                .min(u64::from(self.config.bandwidth_burst_bytes));
        }
        self.now = tick;
        Ok(())
    }

    pub fn enqueue(&mut self, datagram: AfcDatagram) -> FaultSendOutcome {
        self.metrics.injection_attempts = self.metrics.injection_attempts.saturating_add(1);
        self.metrics.injected_bytes = self
            .metrics
            .injected_bytes
            .saturating_add(datagram.len() as u64);

        if self.is_disconnected() {
            self.metrics.disconnected_send_attempts =
                self.metrics.disconnected_send_attempts.saturating_add(1);
            return FaultSendOutcome::Disconnected(datagram);
        }
        if self.scheduled.len() >= self.config.queue_capacity_packets {
            self.metrics.queue_full_events = self.metrics.queue_full_events.saturating_add(1);
            return FaultSendOutcome::Full(datagram);
        }
        if self.roll(self.config.loss_per_10k) {
            self.metrics.dropped_by_loss = self.metrics.dropped_by_loss.saturating_add(1);
            return FaultSendOutcome::DroppedByLoss;
        }

        let duplicate = self.roll(self.config.duplication_per_10k);
        let original_bytes = datagram.len() as u64;
        self.schedule_copy(datagram.clone(), false);
        let mut scheduled_copies = 1;

        if duplicate {
            if self.scheduled.len() < self.config.queue_capacity_packets {
                self.schedule_copy(datagram, true);
                scheduled_copies = 2;
            } else {
                // The original was accepted, while the requested duplicate was
                // explicitly refused by the same bounded queue.
                self.metrics.queue_full_events = self.metrics.queue_full_events.saturating_add(1);
            }
        }

        self.metrics.accepted_copies = self
            .metrics
            .accepted_copies
            .saturating_add(u64::from(scheduled_copies));
        self.metrics.accepted_bytes = self
            .metrics
            .accepted_bytes
            .saturating_add(original_bytes.saturating_mul(u64::from(scheduled_copies)));
        self.metrics.pending_high_water = self.metrics.pending_high_water.max(self.scheduled.len());
        FaultSendOutcome::Accepted { scheduled_copies }
    }

    pub fn try_receive(&mut self) -> FaultReceiveOutcome {
        if self.is_disconnected() {
            self.metrics.disconnected_receive_attempts =
                self.metrics.disconnected_receive_attempts.saturating_add(1);
            return FaultReceiveOutcome::Disconnected;
        }

        let Some(next) = self.scheduled.peek() else {
            return FaultReceiveOutcome::Empty;
        };
        if next.release_tick > self.now {
            return FaultReceiveOutcome::Empty;
        }
        let packet_bytes = next.datagram.len() as u64;
        if self.config.bandwidth_bytes_per_tick > 0 && packet_bytes > self.bandwidth_tokens {
            return FaultReceiveOutcome::Empty;
        }

        let scheduled = self
            .scheduled
            .pop()
            .expect("a previously observed scheduled datagram remains present");
        if self.config.bandwidth_bytes_per_tick > 0 {
            self.bandwidth_tokens -= packet_bytes;
        }
        self.metrics.delivered_datagrams = self.metrics.delivered_datagrams.saturating_add(1);
        self.metrics.delivered_bytes = self.metrics.delivered_bytes.saturating_add(packet_bytes);
        FaultReceiveOutcome::Received(scheduled.datagram)
    }

    fn schedule_copy(&mut self, datagram: AfcDatagram, duplicate: bool) {
        if duplicate {
            self.metrics.duplicate_copies = self.metrics.duplicate_copies.saturating_add(1);
        }

        let jittered_latency = self.jittered_latency();
        let reordered = self.roll(self.config.reorder_per_10k);
        let reorder_delay = if reordered {
            self.metrics.reordered_copies = self.metrics.reordered_copies.saturating_add(1);
            1 + self.random_below(u64::from(self.config.max_reorder_extra_ticks))
        } else {
            0
        };
        let release_tick = self
            .now
            .saturating_add(jittered_latency)
            .saturating_add(reorder_delay);
        let release_tick = align_to_burst(
            release_tick,
            u64::from(self.config.delivery_burst_interval_ticks),
        );
        let ordinal = self.next_ordinal;
        self.next_ordinal = self.next_ordinal.wrapping_add(1);
        self.scheduled.push(ScheduledDatagram {
            release_tick,
            ordinal,
            datagram,
        });
    }

    fn jittered_latency(&mut self) -> u64 {
        let base = u64::from(self.config.base_latency_ticks);
        let jitter = u64::from(self.config.jitter_ticks);
        if jitter == 0 {
            return base;
        }
        let offset = self.random_below(jitter.saturating_mul(2).saturating_add(1));
        if offset >= jitter {
            base.saturating_add(offset - jitter)
        } else {
            base.saturating_sub(jitter - offset)
        }
    }

    fn roll(&mut self, rate_per_10k: u16) -> bool {
        if rate_per_10k == 0 {
            return false;
        }
        self.random_below(u64::from(PROBABILITY_SCALE)) < u64::from(rate_per_10k)
    }

    fn random_below(&mut self, exclusive_maximum: u64) -> u64 {
        debug_assert!(exclusive_maximum > 0);
        self.rng.next_u64() % exclusive_maximum
    }
}

fn align_to_burst(tick: NetworkTick, interval: u64) -> NetworkTick {
    if interval <= 1 {
        return tick;
    }
    let remainder = tick % interval;
    if remainder == 0 {
        tick
    } else {
        tick.saturating_add(interval - remainder)
    }
}

struct ScheduledDatagram {
    release_tick: NetworkTick,
    ordinal: u64,
    datagram: AfcDatagram,
}

impl PartialEq for ScheduledDatagram {
    fn eq(&self, other: &Self) -> bool {
        self.release_tick == other.release_tick && self.ordinal == other.ordinal
    }
}

impl Eq for ScheduledDatagram {}

impl PartialOrd for ScheduledDatagram {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ScheduledDatagram {
    fn cmp(&self, other: &Self) -> Ordering {
        // BinaryHeap is a max-heap, so reverse both keys. Earlier release ticks
        // win, and insertion order is stable for equal release ticks.
        other
            .release_tick
            .cmp(&self.release_tick)
            .then_with(|| other.ordinal.cmp(&self.ordinal))
    }
}

#[derive(Clone, Copy)]
struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut value = self.state;
        value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        value ^ (value >> 31)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::{Duration, Instant};

    fn packet(tag: u8, size: usize) -> AfcDatagram {
        AfcDatagram::try_from_slice(&vec![tag; size]).unwrap()
    }

    fn receive_udp_until(endpoint: &mut UdpEndpoint) -> ReceiveOutcome {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match endpoint.try_receive() {
                ReceiveOutcome::Empty if Instant::now() < deadline => thread::yield_now(),
                outcome => return outcome,
            }
        }
    }

    fn drain_fault(layer: &mut DeterministicFaultLayer) -> Vec<Vec<u8>> {
        let mut packets = Vec::new();
        loop {
            match layer.try_receive() {
                FaultReceiveOutcome::Received(packet) => packets.push(packet.as_slice().to_vec()),
                FaultReceiveOutcome::Empty | FaultReceiveOutcome::Disconnected => return packets,
            }
        }
    }

    fn run_duplex_fault_scenario(
        config: FaultLabConfig,
        send_ticks: u64,
        flush_tick: u64,
    ) -> (Vec<Vec<u8>>, Vec<Vec<u8>>, FaultLabMetrics) {
        let (lab, mut a, mut b) = DeterministicNetworkLab::pair(config).unwrap();
        let mut received_by_a = Vec::new();
        let mut received_by_b = Vec::new();
        for tick in 0..send_ticks {
            lab.advance_to(tick).unwrap();
            assert_eq!(a.try_send(packet(tick as u8, 32)), SendOutcome::Sent);
            assert_eq!(
                b.try_send(packet((tick as u8).wrapping_add(97), 48)),
                SendOutcome::Sent
            );
            while let ReceiveOutcome::Received(datagram) = a.try_receive() {
                received_by_a.push(datagram.as_slice().to_vec());
            }
            while let ReceiveOutcome::Received(datagram) = b.try_receive() {
                received_by_b.push(datagram.as_slice().to_vec());
            }
        }
        for tick in send_ticks..=flush_tick {
            lab.advance_to(tick).unwrap();
            while let ReceiveOutcome::Received(datagram) = a.try_receive() {
                received_by_a.push(datagram.as_slice().to_vec());
            }
            while let ReceiveOutcome::Received(datagram) = b.try_receive() {
                received_by_b.push(datagram.as_slice().to_vec());
            }
        }
        (received_by_a, received_by_b, lab.metrics())
    }

    #[test]
    fn channel_contract_is_exact_and_stable() {
        assert_eq!(AFC_CHANNELS.len(), 5);
        for (index, channel) in AfcChannel::ALL.into_iter().enumerate() {
            assert_eq!(channel.wire_id(), (index + 1) as u8);
            assert_eq!(AfcChannel::try_from(channel.wire_id()), Ok(channel));
            assert_eq!(channel.metadata(), AFC_CHANNELS[index]);
        }
        assert_eq!(AfcChannel::try_from(0), Err(UnknownChannel(0)));
        assert_eq!(AfcChannel::try_from(6), Err(UnknownChannel(6)));

        assert_eq!(
            AfcChannel::Control.metadata(),
            ChannelMetadata {
                channel: AfcChannel::Control,
                delivery: DeliverySemantics::OrderedReliable,
                direction: TrafficDirection::Bidirectional,
            }
        );
        assert_eq!(
            AfcChannel::Input.metadata().delivery,
            DeliverySemantics::SequencedUnreliable
        );
        assert_eq!(
            AfcChannel::State.metadata(),
            ChannelMetadata {
                channel: AfcChannel::State,
                delivery: DeliverySemantics::SequencedUnreliable,
                direction: TrafficDirection::AuthorityToClient,
            }
        );
        assert_eq!(
            AfcChannel::Resync.metadata().delivery,
            DeliverySemantics::UnorderedReliable
        );
        assert_eq!(
            AfcChannel::Result.metadata().delivery,
            DeliverySemantics::OrderedReliable
        );
        for channel in [AfcChannel::State, AfcChannel::Resync, AfcChannel::Result] {
            assert!(channel.metadata().permits_sender(EndpointRole::Authority));
            assert!(!channel.metadata().permits_sender(EndpointRole::Client));
        }
        for channel in [AfcChannel::Control, AfcChannel::Input] {
            assert!(channel.metadata().permits_sender(EndpointRole::Authority));
            assert!(channel.metadata().permits_sender(EndpointRole::Client));
        }
    }

    #[test]
    fn datagram_accepts_only_bounded_sizes_and_canonicalizes_padding() {
        let empty = AfcDatagram::try_from_slice(&[]).unwrap();
        assert!(empty.is_empty());
        let maximum = packet(0xA5, MAX_AFC_DATAGRAM_BYTES);
        assert_eq!(maximum.len(), MAX_AFC_DATAGRAM_BYTES);
        assert!(maximum.as_slice().iter().all(|byte| *byte == 0xA5));
        assert_eq!(maximum.clone(), maximum);

        let oversized = vec![0; MAX_AFC_DATAGRAM_BYTES + 1];
        assert_eq!(
            AfcDatagram::try_from_slice(&oversized),
            Err(DatagramSizeError {
                received: MAX_AFC_DATAGRAM_BYTES + 1,
                maximum: MAX_AFC_DATAGRAM_BYTES,
            })
        );
    }

    #[test]
    fn arbitrary_input_sizes_never_cross_the_datagram_bound() {
        let mut state = 7_u64;
        for _ in 0..10_000 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            let len = (state as usize) % 4_000;
            let bytes = vec![(state >> 32) as u8; len];
            let result = AfcDatagram::try_from_slice(&bytes);
            assert_eq!(result.is_ok(), len <= MAX_AFC_DATAGRAM_BYTES);
        }
    }

    #[test]
    fn in_process_duplex_is_bounded_and_tracks_high_water() {
        let (mut a, mut b) = InProcessEndpoint::pair(2).unwrap();
        let one = packet(1, 10);
        let two = packet(2, 20);
        assert_eq!(a.try_send(one.clone()), SendOutcome::Sent);
        assert_eq!(a.try_send(two.clone()), SendOutcome::Sent);
        assert_eq!(a.try_send(packet(3, 30)), SendOutcome::Full(packet(3, 30)));

        let metrics = a.metrics().outbound;
        assert_eq!(metrics.capacity_packets, 2);
        assert_eq!(metrics.depth_packets, 2);
        assert_eq!(metrics.high_water_packets, 2);
        assert_eq!(metrics.send_attempts, 3);
        assert_eq!(metrics.sent_packets, 2);
        assert_eq!(metrics.sent_bytes, 30);
        assert_eq!(metrics.full_send_attempts, 1);

        assert_eq!(b.try_receive(), ReceiveOutcome::Received(one));
        assert_eq!(b.try_receive(), ReceiveOutcome::Received(two));
        assert_eq!(b.try_receive(), ReceiveOutcome::Empty);
        let metrics = b.metrics().inbound;
        assert_eq!(metrics.depth_packets, 0);
        assert_eq!(metrics.received_packets, 2);
        assert_eq!(metrics.received_bytes, 30);
        assert_eq!(metrics.empty_receive_attempts, 1);
    }

    #[test]
    fn in_process_directions_are_independent() {
        let (mut a, mut b) = InProcessEndpoint::pair(1).unwrap();
        assert_eq!(a.try_send(packet(1, 1)), SendOutcome::Sent);
        assert_eq!(b.try_send(packet(2, 1)), SendOutcome::Sent);
        assert_eq!(a.try_receive(), ReceiveOutcome::Received(packet(2, 1)));
        assert_eq!(b.try_receive(), ReceiveOutcome::Received(packet(1, 1)));
        assert_eq!(a.metrics().outbound.high_water_packets, 1);
        assert_eq!(a.metrics().inbound.high_water_packets, 1);
    }

    #[test]
    fn in_process_disconnects_are_explicit_and_preserve_failed_packet() {
        let (mut a, b) = InProcessEndpoint::pair(1).unwrap();
        drop(b);
        let rejected = packet(9, 9);
        assert_eq!(
            a.try_send(rejected.clone()),
            SendOutcome::Disconnected(rejected)
        );
        assert_eq!(a.try_receive(), ReceiveOutcome::Disconnected);
        let metrics = a.metrics();
        assert_eq!(metrics.outbound.disconnected_send_attempts, 1);
        assert_eq!(metrics.inbound.disconnected_receive_attempts, 1);
        assert_eq!(metrics.outbound.depth_packets, 0);
    }

    #[test]
    fn dropping_receiver_discards_a_full_queue_and_reports_disconnected() {
        let (mut a, b) = InProcessEndpoint::pair(1).unwrap();
        assert_eq!(a.try_send(packet(1, 1)), SendOutcome::Sent);
        assert_eq!(a.metrics().outbound.depth_packets, 1);
        drop(b);

        let rejected = packet(2, 2);
        assert_eq!(
            a.try_send(rejected.clone()),
            SendOutcome::Disconnected(rejected)
        );
        let metrics = a.metrics().outbound;
        assert_eq!(metrics.depth_packets, 0);
        assert_eq!(metrics.discarded_on_receiver_drop, 1);
        assert_eq!(metrics.disconnected_send_attempts, 1);
    }

    #[test]
    fn in_process_configuration_is_defensively_bounded() {
        assert!(matches!(
            InProcessEndpoint::pair(0),
            Err(InProcessConfigError::ZeroCapacity)
        ));
        assert!(matches!(
            InProcessEndpoint::pair(MAX_IN_PROCESS_QUEUE_PACKETS + 1),
            Err(InProcessConfigError::CapacityExceeded { .. })
        ));
    }

    #[test]
    fn udp_loopback_round_trips_maximum_datagram_nonblocking() {
        let (mut a, mut b) = UdpEndpoint::loopback_pair().unwrap();
        assert_eq!(b.try_receive(), ReceiveOutcome::Empty);
        let maximum = packet(0x3C, MAX_AFC_DATAGRAM_BYTES);
        assert_eq!(a.try_send(maximum.clone()), SendOutcome::Sent);
        assert_eq!(receive_udp_until(&mut b), ReceiveOutcome::Received(maximum));
        assert_eq!(a.metrics().sent_bytes, MAX_AFC_DATAGRAM_BYTES as u64);
        assert_eq!(b.metrics().received_bytes, MAX_AFC_DATAGRAM_BYTES as u64);
        assert!(b.metrics().receive_would_block >= 1);
    }

    #[test]
    fn udp_rejects_oversized_datagram_without_allocating_from_its_length() {
        let raw_sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let receiver_socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        raw_sender
            .connect(receiver_socket.local_addr().unwrap())
            .unwrap();
        receiver_socket
            .connect(raw_sender.local_addr().unwrap())
            .unwrap();
        let mut receiver = UdpEndpoint::from_connected_socket(receiver_socket).unwrap();

        raw_sender
            .send(&vec![0xDD; MAX_AFC_DATAGRAM_BYTES + 500])
            .unwrap();
        assert_eq!(
            receive_udp_until(&mut receiver),
            ReceiveOutcome::Oversized {
                observed_at_least: MAX_AFC_DATAGRAM_BYTES + 1,
            }
        );
        assert_eq!(receiver.metrics().oversized_datagrams, 1);
        assert_eq!(receiver.metrics().received_datagrams, 0);
    }

    #[test]
    fn fault_configuration_rejects_unsafe_values() {
        assert_eq!(
            FaultConfig {
                queue_capacity_packets: 0,
                ..FaultConfig::default()
            }
            .validate(),
            Err(FaultConfigError::ZeroQueueCapacity)
        );
        assert!(matches!(
            FaultConfig {
                loss_per_10k: PROBABILITY_SCALE + 1,
                ..FaultConfig::default()
            }
            .validate(),
            Err(FaultConfigError::InvalidProbability { .. })
        ));
        assert_eq!(
            FaultConfig {
                reorder_per_10k: 1,
                ..FaultConfig::default()
            }
            .validate(),
            Err(FaultConfigError::MissingReorderDelay)
        );
        assert_eq!(
            FaultConfig {
                bandwidth_bytes_per_tick: 1,
                bandwidth_burst_bytes: (MAX_AFC_DATAGRAM_BYTES - 1) as u32,
                ..FaultConfig::default()
            }
            .validate(),
            Err(FaultConfigError::BandwidthBurstTooSmall)
        );
        assert_eq!(
            FaultConfig {
                disconnect: Some(DisconnectWindow {
                    start_tick: 10,
                    reconnect_tick: Some(10),
                }),
                ..FaultConfig::default()
            }
            .validate(),
            Err(FaultConfigError::InvalidDisconnectWindow)
        );
    }

    #[test]
    fn fault_layer_is_reproducible_for_seed_config_and_tick_stream() {
        let config = FaultConfig {
            base_latency_ticks: 6,
            jitter_ticks: 3,
            loss_per_10k: 1_200,
            duplication_per_10k: 2_000,
            reorder_per_10k: 2_500,
            max_reorder_extra_ticks: 5,
            queue_capacity_packets: 512,
            ..FaultConfig::default()
        };
        let mut left = DeterministicFaultLayer::new(config, 0xAFC0_1234).unwrap();
        let mut right = DeterministicFaultLayer::new(config, 0xAFC0_1234).unwrap();

        let mut left_output = Vec::new();
        let mut right_output = Vec::new();
        for tick in 0..200 {
            left.advance_to(tick).unwrap();
            right.advance_to(tick).unwrap();
            if tick < 100 {
                let datagram = packet(tick as u8, 8);
                assert_eq!(left.enqueue(datagram.clone()), right.enqueue(datagram));
            }
            left_output.extend(drain_fault(&mut left));
            right_output.extend(drain_fault(&mut right));
        }

        assert_eq!(left_output, right_output);
        assert_eq!(left.metrics(), right.metrics());
        assert_eq!(left.seed(), 0xAFC0_1234);
        assert!(left.metrics().dropped_by_loss > 0);
        assert!(left.metrics().duplicate_copies > 0);
        assert!(left.metrics().reordered_copies > 0);
    }

    #[test]
    fn fault_loss_and_duplication_have_exact_extreme_behavior() {
        let mut loss = DeterministicFaultLayer::new(
            FaultConfig {
                loss_per_10k: PROBABILITY_SCALE,
                ..FaultConfig::default()
            },
            1,
        )
        .unwrap();
        assert_eq!(loss.enqueue(packet(1, 10)), FaultSendOutcome::DroppedByLoss);
        assert_eq!(loss.try_receive(), FaultReceiveOutcome::Empty);
        assert_eq!(loss.metrics().dropped_by_loss, 1);

        let mut duplicate = DeterministicFaultLayer::new(
            FaultConfig {
                duplication_per_10k: PROBABILITY_SCALE,
                queue_capacity_packets: 2,
                ..FaultConfig::default()
            },
            2,
        )
        .unwrap();
        assert_eq!(
            duplicate.enqueue(packet(2, 10)),
            FaultSendOutcome::Accepted {
                scheduled_copies: 2
            }
        );
        assert_eq!(drain_fault(&mut duplicate).len(), 2);
        assert_eq!(duplicate.metrics().duplicate_copies, 1);
    }

    #[test]
    fn fault_latency_jitter_never_delivers_outside_configured_bounds() {
        let mut layer = DeterministicFaultLayer::new(
            FaultConfig {
                base_latency_ticks: 10,
                jitter_ticks: 3,
                queue_capacity_packets: 64,
                ..FaultConfig::default()
            },
            3,
        )
        .unwrap();
        for tag in 0..32 {
            assert!(matches!(
                layer.enqueue(packet(tag, 1)),
                FaultSendOutcome::Accepted { .. }
            ));
        }
        layer.advance_to(6).unwrap();
        assert_eq!(layer.try_receive(), FaultReceiveOutcome::Empty);
        layer.advance_to(7).unwrap();
        assert!(!drain_fault(&mut layer).is_empty());
        layer.advance_to(13).unwrap();
        let _ = drain_fault(&mut layer);
        assert_eq!(layer.metrics().delivered_datagrams, 32);
        assert_eq!(layer.metrics().pending_datagrams, 0);
    }

    #[test]
    fn reorder_fault_can_change_delivery_order() {
        let config = FaultConfig {
            reorder_per_10k: PROBABILITY_SCALE,
            max_reorder_extra_ticks: 8,
            queue_capacity_packets: 32,
            ..FaultConfig::default()
        };
        let mut found_reordered_seed = false;
        for seed in 0..64 {
            let mut layer = DeterministicFaultLayer::new(config, seed).unwrap();
            for tag in 0..16 {
                layer.enqueue(packet(tag, 1));
            }
            layer.advance_to(8).unwrap();
            let output: Vec<u8> = drain_fault(&mut layer)
                .into_iter()
                .map(|bytes| bytes[0])
                .collect();
            if output != (0_u8..16).collect::<Vec<_>>() {
                found_reordered_seed = true;
                break;
            }
        }
        assert!(found_reordered_seed);
    }

    #[test]
    fn bandwidth_token_bucket_is_tick_driven_and_bounded() {
        let mut layer = DeterministicFaultLayer::new(
            FaultConfig {
                bandwidth_bytes_per_tick: 100,
                bandwidth_burst_bytes: MAX_AFC_DATAGRAM_BYTES as u32,
                queue_capacity_packets: 4,
                ..FaultConfig::default()
            },
            4,
        )
        .unwrap();
        layer.enqueue(packet(1, MAX_AFC_DATAGRAM_BYTES));
        layer.enqueue(packet(2, MAX_AFC_DATAGRAM_BYTES));
        assert!(matches!(
            layer.try_receive(),
            FaultReceiveOutcome::Received(_)
        ));
        assert_eq!(layer.try_receive(), FaultReceiveOutcome::Empty);
        layer.advance_to(11).unwrap();
        assert_eq!(layer.try_receive(), FaultReceiveOutcome::Empty);
        layer.advance_to(12).unwrap();
        assert!(matches!(
            layer.try_receive(),
            FaultReceiveOutcome::Received(_)
        ));
        assert_eq!(layer.metrics().pending_datagrams, 0);
        assert_eq!(layer.metrics().pending_high_water, 2);
    }

    #[test]
    fn burst_delivery_aligns_packets_without_wall_clock_time() {
        let mut layer = DeterministicFaultLayer::new(
            FaultConfig {
                delivery_burst_interval_ticks: 5,
                ..FaultConfig::default()
            },
            5,
        )
        .unwrap();
        layer.advance_to(1).unwrap();
        layer.enqueue(packet(1, 1));
        layer.advance_to(4).unwrap();
        assert_eq!(layer.try_receive(), FaultReceiveOutcome::Empty);
        layer.advance_to(5).unwrap();
        assert_eq!(
            layer.try_receive(),
            FaultReceiveOutcome::Received(packet(1, 1))
        );
    }

    #[test]
    fn disconnect_purges_in_flight_data_and_reconnects_at_exact_tick() {
        let mut layer = DeterministicFaultLayer::new(
            FaultConfig {
                base_latency_ticks: 20,
                disconnect: Some(DisconnectWindow {
                    start_tick: 10,
                    reconnect_tick: Some(15),
                }),
                ..FaultConfig::default()
            },
            6,
        )
        .unwrap();
        layer.enqueue(packet(1, 1));
        layer.advance_to(10).unwrap();
        assert_eq!(layer.metrics().purged_on_disconnect, 1);
        assert_eq!(layer.metrics().pending_datagrams, 0);
        let rejected = packet(2, 2);
        assert_eq!(
            layer.enqueue(rejected.clone()),
            FaultSendOutcome::Disconnected(rejected)
        );
        assert_eq!(layer.try_receive(), FaultReceiveOutcome::Disconnected);
        layer.advance_to(15).unwrap();
        assert!(!layer.is_disconnected());
        assert!(matches!(
            layer.enqueue(packet(3, 3)),
            FaultSendOutcome::Accepted { .. }
        ));
        layer.advance_to(35).unwrap();
        assert_eq!(
            layer.try_receive(),
            FaultReceiveOutcome::Received(packet(3, 3))
        );
    }

    #[test]
    fn fault_queue_full_is_explicit_and_duplicate_cannot_overflow() {
        let mut layer = DeterministicFaultLayer::new(
            FaultConfig {
                base_latency_ticks: 10,
                duplication_per_10k: PROBABILITY_SCALE,
                queue_capacity_packets: 1,
                ..FaultConfig::default()
            },
            7,
        )
        .unwrap();
        assert_eq!(
            layer.enqueue(packet(1, 1)),
            FaultSendOutcome::Accepted {
                scheduled_copies: 1
            }
        );
        let rejected = packet(2, 2);
        assert_eq!(
            layer.enqueue(rejected.clone()),
            FaultSendOutcome::Full(rejected)
        );
        assert_eq!(layer.metrics().pending_datagrams, 1);
        assert_eq!(layer.metrics().pending_high_water, 1);
        assert_eq!(layer.metrics().queue_full_events, 2);
    }

    #[test]
    fn moving_fault_time_backwards_is_rejected_without_mutation() {
        let mut layer = DeterministicFaultLayer::new(FaultConfig::default(), 8).unwrap();
        layer.advance_to(12).unwrap();
        assert_eq!(
            layer.advance_to(11),
            Err(TickWentBackwards {
                current: 12,
                requested: 11,
            })
        );
        assert_eq!(layer.current_tick(), 12);
    }

    #[test]
    fn duplex_fault_lab_is_a_normal_nonblocking_transport() {
        let (lab, mut a, mut b) = DeterministicNetworkLab::pair(FaultLabConfig::new(
            FaultConfig {
                base_latency_ticks: 3,
                loss_per_10k: PROBABILITY_SCALE,
                ..FaultConfig::default()
            },
            FaultConfig {
                base_latency_ticks: 2,
                ..FaultConfig::default()
            },
            10,
            20,
        ))
        .unwrap();

        // Datagram loss is invisible to a sender, just like UDP.
        assert_eq!(a.try_send(packet(1, 16)), SendOutcome::Sent);
        assert_eq!(b.try_send(packet(2, 24)), SendOutcome::Sent);
        lab.advance_to(1).unwrap();
        assert_eq!(a.try_receive(), ReceiveOutcome::Empty);
        lab.advance_to(2).unwrap();
        assert_eq!(a.try_receive(), ReceiveOutcome::Received(packet(2, 24)));
        lab.advance_to(3).unwrap();
        assert_eq!(b.try_receive(), ReceiveOutcome::Empty);

        let metrics = lab.metrics();
        assert_eq!(metrics.current_tick, 3);
        assert_eq!(metrics.a_to_b.dropped_by_loss, 1);
        assert_eq!(metrics.a_to_b.pending_datagrams, 0);
        assert_eq!(metrics.b_to_a.delivered_datagrams, 1);
    }

    #[test]
    fn net_typical_and_degraded_profiles_are_reproducible_and_bounded() {
        // Integer network ticks deliberately avoid wall-clock rounding. The
        // degraded profile uses asymmetric base delay to represent nine RTT
        // ticks exactly rather than rounding 4.5 one-way ticks twice.
        let degraded = FaultLabConfig::net_degraded_60hz(0xDE6A_ADED);
        for (base_latency_ticks, direction) in [(4, degraded.a_to_b), (5, degraded.b_to_a)] {
            assert_eq!(direction.base_latency_ticks, base_latency_ticks);
            assert_eq!(direction.jitter_ticks, 2);
            assert_eq!(direction.loss_per_10k, 300);
            assert_eq!(direction.duplication_per_10k, 100);
            assert_eq!(direction.reorder_per_10k, 100);
            assert_eq!(direction.max_reorder_extra_ticks, 1);
        }

        for config in [FaultLabConfig::net_typical_60hz(0x711C_A1), degraded] {
            let first = run_duplex_fault_scenario(config, 600, 700);
            let second = run_duplex_fault_scenario(config, 600, 700);
            assert_eq!(first, second);

            let (received_by_a, received_by_b, metrics) = first;
            assert!(!received_by_a.is_empty());
            assert!(!received_by_b.is_empty());
            assert!(metrics.a_to_b.dropped_by_loss + metrics.b_to_a.dropped_by_loss > 0);
            assert!(metrics.a_to_b.pending_high_water <= config.a_to_b.queue_capacity_packets);
            assert!(metrics.b_to_a.pending_high_water <= config.b_to_a.queue_capacity_packets);
            assert_eq!(metrics.a_to_b.pending_datagrams, 0);
            assert_eq!(metrics.b_to_a.pending_datagrams, 0);
            assert_eq!(metrics.a_to_b.queue_full_events, 0);
            assert_eq!(metrics.b_to_a.queue_full_events, 0);
        }
    }

    #[test]
    fn duplex_reorder_changes_arrival_order_without_losing_bounds() {
        let config = FaultConfig {
            reorder_per_10k: PROBABILITY_SCALE,
            max_reorder_extra_ticks: 12,
            queue_capacity_packets: 32,
            ..FaultConfig::default()
        };
        let (lab, mut a, mut b) =
            DeterministicNetworkLab::pair(FaultLabConfig::symmetric(config, 0x0DDE_1234)).unwrap();
        for tag in 0..24 {
            assert_eq!(a.try_send(packet(tag, 1)), SendOutcome::Sent);
        }
        lab.advance_to(12).unwrap();
        let mut order = Vec::new();
        while let ReceiveOutcome::Received(datagram) = b.try_receive() {
            order.push(datagram.as_slice()[0]);
        }
        assert_eq!(order.len(), 24);
        assert_ne!(order, (0_u8..24).collect::<Vec<_>>());
        assert_eq!(lab.metrics().a_to_b.reordered_copies, 24);
        assert_eq!(lab.metrics().a_to_b.pending_high_water, 24);
    }

    #[test]
    fn duplex_disconnect_is_exact_purges_inflight_and_peer_drop_is_observable() {
        let disconnected = FaultConfig {
            base_latency_ticks: 20,
            disconnect: Some(DisconnectWindow {
                start_tick: 10,
                reconnect_tick: Some(15),
            }),
            queue_capacity_packets: 4,
            ..FaultConfig::default()
        };
        let (lab, mut a, mut b) =
            DeterministicNetworkLab::pair(FaultLabConfig::symmetric(disconnected, 0xD15C_0))
                .unwrap();
        assert_eq!(a.try_send(packet(1, 1)), SendOutcome::Sent);
        assert_eq!(b.try_send(packet(2, 1)), SendOutcome::Sent);
        lab.advance_to(10).unwrap();
        assert_eq!(a.try_receive(), ReceiveOutcome::Disconnected);
        assert_eq!(b.try_receive(), ReceiveOutcome::Disconnected);
        assert!(matches!(
            a.try_send(packet(3, 1)),
            SendOutcome::Disconnected(_)
        ));
        let metrics = lab.metrics();
        assert_eq!(metrics.a_to_b.purged_on_disconnect, 1);
        assert_eq!(metrics.b_to_a.purged_on_disconnect, 1);

        lab.advance_to(15).unwrap();
        assert_eq!(a.try_send(packet(4, 1)), SendOutcome::Sent);
        drop(b);
        assert!(matches!(
            a.try_send(packet(5, 1)),
            SendOutcome::Disconnected(_)
        ));
        assert_eq!(a.try_receive(), ReceiveOutcome::Disconnected);
        let metrics = lab.metrics();
        assert!(!metrics.endpoint_b_alive);
        assert_eq!(metrics.endpoint_a.peer_dropped_send_attempts, 1);
        assert_eq!(metrics.endpoint_a.peer_dropped_receive_attempts, 1);
    }
}
