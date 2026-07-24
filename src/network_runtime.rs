//! Transport-independent AFC packet and peer-session runtime.
//!
//! The runtime owns one already-connected datagram endpoint. Simulation code sees
//! only typed [`RuntimeEvent`] values and queues typed codec messages; endpoint,
//! packet-envelope, retry, ordering, and abuse details remain inside this module.

use std::io;

use crate::network_codec::{
    EncodeError, Handshake, ResultIdentifier, WireMessage, decode_packet, encode_packet,
};
use crate::network_io::{
    AfcChannel, AfcDatagram, DeliverySemantics, EndpointRole, MAX_AFC_DATAGRAM_BYTES,
    NonBlockingDatagramEndpoint, ReceiveOutcome, SendOutcome,
};
use crate::network_protocol::{
    ClockReply, CompatibilityId, DisconnectMessage, MAX_RESYNC_CHUNKS, MatchId, PeerId,
    ProtocolChannel, ProtocolValidationError, ResyncChunk, SimTick, StartMessage,
};
use crate::session::{
    AppliedInitialSync, AuthoritySessionGate, ClientSession, ConfirmedSessionResult,
    DEFAULT_COUNTDOWN_LEAD_TICKS, SessionError,
};

pub const MAX_RUNTIME_QUEUE_MESSAGES: usize = 64;
pub const MAX_RELIABLE_REORDER_MESSAGES: usize = 32;
pub const MAX_RECENT_RESYNC_TRANSFERS: usize = 2;
pub const MAX_RUNTIME_DATAGRAMS_PER_PUMP: usize = 64;
const RESYNC_CHUNK_SEEN_WORDS: usize = MAX_RESYNC_CHUNKS.div_ceil(u64::BITS as usize);

const RUNTIME_MAGIC: [u8; 4] = *b"AFCR";
const RUNTIME_ENVELOPE_VERSION: u8 = 1;
const RUNTIME_ENVELOPE_BYTES: usize = 18;
const FLAG_MESSAGE: u8 = 1 << 0;
const FLAG_ACK: u8 = 1 << 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerRole {
    Client,
    Authority,
}

impl PeerRole {
    const fn endpoint_role(self) -> EndpointRole {
        match self {
            Self::Client => EndpointRole::Client,
            Self::Authority => EndpointRole::Authority,
        }
    }

    const fn remote(self) -> Self {
        match self {
            Self::Client => Self::Authority,
            Self::Authority => Self::Client,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeConfig {
    pub inbound_capacity: usize,
    pub outbound_capacity: usize,
    pub reliable_reorder_capacity: usize,
    pub max_receive_datagrams_per_pump: usize,
    pub max_send_datagrams_per_pump: usize,
    pub reliable_retry_interval_ticks: u32,
    pub reliable_max_attempts: u16,
    pub abuse_warning_threshold: u32,
    pub abuse_disconnect_threshold: u32,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            inbound_capacity: 32,
            outbound_capacity: 32,
            reliable_reorder_capacity: 16,
            max_receive_datagrams_per_pump: 32,
            max_send_datagrams_per_pump: 32,
            reliable_retry_interval_ticks: 6,
            reliable_max_attempts: 32,
            abuse_warning_threshold: 8,
            abuse_disconnect_threshold: 24,
        }
    }
}

impl RuntimeConfig {
    pub fn validate(self) -> Result<(), RuntimeConfigError> {
        if self.inbound_capacity == 0
            || self.outbound_capacity == 0
            || self.reliable_reorder_capacity == 0
            || self.max_receive_datagrams_per_pump == 0
            || self.max_send_datagrams_per_pump == 0
            || self.reliable_retry_interval_ticks == 0
            || self.reliable_max_attempts == 0
            || self.abuse_warning_threshold == 0
            || self.abuse_disconnect_threshold < self.abuse_warning_threshold
        {
            return Err(RuntimeConfigError::ZeroOrInvalidLimit);
        }
        if self.inbound_capacity > MAX_RUNTIME_QUEUE_MESSAGES
            || self.outbound_capacity > MAX_RUNTIME_QUEUE_MESSAGES
            || self.reliable_reorder_capacity > MAX_RELIABLE_REORDER_MESSAGES
            || self.max_receive_datagrams_per_pump > MAX_RUNTIME_DATAGRAMS_PER_PUMP
            || self.max_send_datagrams_per_pump > MAX_RUNTIME_DATAGRAMS_PER_PUMP
        {
            return Err(RuntimeConfigError::CapacityExceeded);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeConfigError {
    ZeroOrInvalidLimit,
    CapacityExceeded,
    SessionRoleMismatch,
}

impl core::fmt::Display for RuntimeConfigError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "invalid AFC runtime configuration: {self:?}")
    }
}

impl std::error::Error for RuntimeConfigError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueDisposition {
    Queued,
    ReplacedLatest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReliableSendHandle {
    slot: u8,
    channel: AfcChannel,
    sequence: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReliableSendStatus {
    Pending,
    Acknowledged,
    Exhausted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeQueueError {
    DirectionDenied {
        role: PeerRole,
        channel: ProtocolChannel,
    },
    OutboundQueueFull,
    Encode(EncodeError),
    DatagramTooLarge,
    InvalidStartMessage(ProtocolValidationError),
}

impl From<EncodeError> for RuntimeQueueError {
    fn from(error: EncodeError) -> Self {
        Self::Encode(error)
    }
}

impl core::fmt::Display for RuntimeQueueError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "AFC runtime queue failed: {self:?}")
    }
}

impl std::error::Error for RuntimeQueueError {}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum RuntimeAbuseSignal {
    #[default]
    None,
    Warning,
    Disconnect,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RuntimeConnectionState {
    #[default]
    Active,
    RemoteDisconnect,
    TransportDisconnected,
    RetryExhausted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeEvent {
    Message(WireMessage),
    SessionError(SessionError),
    TransportDisconnected,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RuntimeMetrics {
    pub received_datagrams: u64,
    pub received_bytes: u64,
    pub decoded_messages: u64,
    pub delivered_messages: u64,
    pub sent_datagrams: u64,
    pub sent_bytes: u64,
    pub reliable_retries: u64,
    pub reliable_acks_sent: u64,
    pub reliable_acks_received: u64,
    pub stale_or_duplicate_unreliable: u64,
    pub duplicate_reliable: u64,
    pub duplicate_results: u64,
    pub duplicate_resync_chunks: u64,
    pub conflicting_idempotent_messages: u64,
    pub malformed_datagrams: u64,
    pub direction_rejections: u64,
    pub decode_rejections: u64,
    pub receive_budget_exhaustions: u64,
    pub send_budget_exhaustions: u64,
    pub inbound_queue_overflows: u64,
    pub outbound_queue_overflows: u64,
    pub reliable_reorder_overflows: u64,
    pub ack_queue_overflows: u64,
    pub send_would_block: u64,
    pub transport_errors: u64,
    pub retry_exhaustions: u64,
    pub abuse_violations: u32,
    pub inbound_high_water: usize,
    pub outbound_high_water: usize,
    pub reliable_high_water: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PumpReport {
    pub tick: SimTick,
    pub received_datagrams: u16,
    pub sent_datagrams: u16,
    pub queued_events: u16,
    pub connection: RuntimeConnectionState,
    pub abuse: RuntimeAbuseSignal,
}

enum SessionBinding {
    None,
    Client(ClientSession),
    Authority {
        gate: AuthoritySessionGate,
        remote_peer: PeerId,
    },
}

struct FixedQueue<T, const N: usize> {
    slots: Box<[Option<T>; N]>,
    head: usize,
    len: usize,
    capacity: usize,
}

impl<T, const N: usize> FixedQueue<T, N> {
    fn new(capacity: usize) -> Self {
        debug_assert!(capacity <= N);
        Self {
            slots: Box::new(std::array::from_fn(|_| None)),
            head: 0,
            len: 0,
            capacity,
        }
    }

    const fn len(&self) -> usize {
        self.len
    }

    const fn is_empty(&self) -> bool {
        self.len == 0
    }

    const fn is_full(&self) -> bool {
        self.len >= self.capacity
    }

    fn push_back(&mut self, value: T) -> Result<(), T> {
        if self.is_full() {
            return Err(value);
        }
        let index = (self.head + self.len) % N;
        self.slots[index] = Some(value);
        self.len += 1;
        Ok(())
    }

    fn push_front(&mut self, value: T) -> Result<(), T> {
        if self.is_full() {
            return Err(value);
        }
        self.head = (self.head + N - 1) % N;
        self.slots[self.head] = Some(value);
        self.len += 1;
        Ok(())
    }

    fn pop_front(&mut self) -> Option<T> {
        if self.is_empty() {
            return None;
        }
        let value = self.slots[self.head].take();
        self.head = (self.head + 1) % N;
        self.len -= 1;
        value
    }

    fn any(&self, mut predicate: impl FnMut(&T) -> bool) -> bool {
        (0..self.len).any(|offset| {
            let Some(value) = self.slots[(self.head + offset) % N].as_ref() else {
                return false;
            };
            predicate(value)
        })
    }
}

#[derive(Clone)]
struct OutboundPacket {
    channel: AfcChannel,
    sequence: u32,
    datagram: AfcDatagram,
}

#[derive(Clone)]
struct ReliableOutbound {
    packet: OutboundPacket,
    last_sent_tick: Option<SimTick>,
    attempts: u16,
    exhausted: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ReliableTerminal {
    channel: AfcChannel,
    sequence: u32,
    status: ReliableSendStatus,
}

#[derive(Clone)]
struct SequencedMessage {
    sequence: u32,
    message: WireMessage,
}

struct ReliableReceiveState {
    next_sequence: u32,
    slots: Box<[Option<SequencedMessage>; MAX_RELIABLE_REORDER_MESSAGES]>,
    len: usize,
}

impl ReliableReceiveState {
    fn new() -> Self {
        Self {
            next_sequence: 0,
            slots: Box::new(std::array::from_fn(|_| None)),
            len: 0,
        }
    }

    fn insert(&mut self, sequence: u32, message: WireMessage, capacity: usize) -> ReliableInsert {
        let distance = sequence.wrapping_sub(self.next_sequence);
        if distance >= (1u32 << 31) {
            return ReliableInsert::AlreadyDelivered;
        }
        if distance as usize >= capacity {
            return ReliableInsert::TooFarAhead;
        }
        let index = sequence as usize % MAX_RELIABLE_REORDER_MESSAGES;
        if let Some(existing) = &self.slots[index] {
            return if existing.sequence == sequence {
                ReliableInsert::DuplicateBuffered
            } else {
                ReliableInsert::TooFarAhead
            };
        }
        self.slots[index] = Some(SequencedMessage { sequence, message });
        self.len += 1;
        ReliableInsert::Inserted
    }

    fn next(&self) -> Option<&SequencedMessage> {
        self.slots[self.next_sequence as usize % MAX_RELIABLE_REORDER_MESSAGES]
            .as_ref()
            .filter(|message| message.sequence == self.next_sequence)
    }

    fn take_next(&mut self) -> Option<SequencedMessage> {
        self.next()?;
        let index = self.next_sequence as usize % MAX_RELIABLE_REORDER_MESSAGES;
        let message = self.slots[index].take();
        self.next_sequence = self.next_sequence.wrapping_add(1);
        self.len = self.len.saturating_sub(1);
        message
    }

    fn restore_next(&mut self, message: SequencedMessage) {
        self.next_sequence = self.next_sequence.wrapping_sub(1);
        let index = message.sequence as usize % MAX_RELIABLE_REORDER_MESSAGES;
        debug_assert!(self.slots[index].is_none());
        self.slots[index] = Some(message);
        self.len += 1;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReliableInsert {
    Inserted,
    AlreadyDelivered,
    DuplicateBuffered,
    TooFarAhead,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingAck {
    channel: AfcChannel,
    sequence: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RecentResyncChunk {
    match_id: crate::network_protocol::MatchId,
    transfer_id: crate::network_protocol::TransferId,
    chunk_index: u16,
    chunk_count: u16,
    fingerprint: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RecentResyncTransfer {
    match_id: crate::network_protocol::MatchId,
    transfer_id: crate::network_protocol::TransferId,
    chunk_count: u16,
    seen: [u64; RESYNC_CHUNK_SEEN_WORDS],
    fingerprints: [u64; MAX_RESYNC_CHUNKS],
}

impl RecentResyncTransfer {
    fn new(chunk: RecentResyncChunk) -> Self {
        let mut transfer = Self {
            match_id: chunk.match_id,
            transfer_id: chunk.transfer_id,
            chunk_count: chunk.chunk_count,
            seen: [0; RESYNC_CHUNK_SEEN_WORDS],
            fingerprints: [0; MAX_RESYNC_CHUNKS],
        };
        transfer.record(chunk);
        transfer
    }

    fn fingerprint(&self, chunk_index: u16) -> Option<u64> {
        let index = usize::from(chunk_index);
        let word = index / u64::BITS as usize;
        let bit = index % u64::BITS as usize;
        (self.seen[word] & (1_u64 << bit) != 0).then_some(self.fingerprints[index])
    }

    fn record(&mut self, chunk: RecentResyncChunk) {
        let index = usize::from(chunk.chunk_index);
        let word = index / u64::BITS as usize;
        let bit = index % u64::BITS as usize;
        self.seen[word] |= 1_u64 << bit;
        self.fingerprints[index] = chunk.fingerprint;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DispatchOutcome {
    Delivered,
    Consumed,
    Blocked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EnvelopeError {
    TooShort,
    BadMagic,
    BadVersion,
    UnknownChannel,
    InvalidFlags,
    NonZeroReserved,
    LengthMismatch,
}

struct DecodedEnvelope<'a> {
    channel: AfcChannel,
    message_sequence: Option<u32>,
    acknowledged_sequence: Option<u32>,
    payload: &'a [u8],
}

/// One peer-to-peer AFC runtime. Authority servers create one instance per peer.
pub struct NetworkRuntime<E: NonBlockingDatagramEndpoint> {
    endpoint: E,
    role: PeerRole,
    compatibility: CompatibilityId,
    session: SessionBinding,
    config: RuntimeConfig,
    inbound: FixedQueue<RuntimeEvent, MAX_RUNTIME_QUEUE_MESSAGES>,
    ack_queue: FixedQueue<PendingAck, MAX_RUNTIME_QUEUE_MESSAGES>,
    reliable_outbound: Box<[Option<ReliableOutbound>; MAX_RUNTIME_QUEUE_MESSAGES]>,
    reliable_terminal: [Option<ReliableTerminal>; MAX_RUNTIME_QUEUE_MESSAGES],
    unreliable_outbound: [Option<OutboundPacket>; 5],
    reliable_receive: [ReliableReceiveState; 5],
    latest_unreliable_sequence: [Option<u32>; 5],
    pending_unreliable_message: [Option<WireMessage>; 5],
    next_outbound_sequence: [u32; 5],
    recent_unordered_sequences: [Option<u32>; MAX_RUNTIME_QUEUE_MESSAGES],
    recent_unordered_cursor: usize,
    recent_resync_transfers: Box<[Option<RecentResyncTransfer>; MAX_RECENT_RESYNC_TRANSFERS]>,
    recent_resync_transfer_cursor: usize,
    last_result: Option<ResultIdentifier>,
    timeout_disconnect_queued: bool,
    connection: RuntimeConnectionState,
    metrics: RuntimeMetrics,
    emitted_abuse: RuntimeAbuseSignal,
}

impl<E: NonBlockingDatagramEndpoint> NetworkRuntime<E> {
    pub fn new(
        endpoint: E,
        role: PeerRole,
        compatibility: CompatibilityId,
        config: RuntimeConfig,
    ) -> Result<Self, RuntimeConfigError> {
        Self::with_session(endpoint, role, compatibility, SessionBinding::None, config)
    }

    pub fn new_client(
        endpoint: E,
        compatibility: CompatibilityId,
        session: ClientSession,
        config: RuntimeConfig,
    ) -> Result<Self, RuntimeConfigError> {
        Self::with_session(
            endpoint,
            PeerRole::Client,
            compatibility,
            SessionBinding::Client(session),
            config,
        )
    }

    pub fn new_authority(
        endpoint: E,
        compatibility: CompatibilityId,
        gate: AuthoritySessionGate,
        remote_peer: PeerId,
        config: RuntimeConfig,
    ) -> Result<Self, RuntimeConfigError> {
        Self::with_session(
            endpoint,
            PeerRole::Authority,
            compatibility,
            SessionBinding::Authority { gate, remote_peer },
            config,
        )
    }

    fn with_session(
        endpoint: E,
        role: PeerRole,
        compatibility: CompatibilityId,
        session: SessionBinding,
        config: RuntimeConfig,
    ) -> Result<Self, RuntimeConfigError> {
        config.validate()?;
        if matches!(&session, SessionBinding::Client(_)) && role != PeerRole::Client
            || matches!(&session, SessionBinding::Authority { .. }) && role != PeerRole::Authority
        {
            return Err(RuntimeConfigError::SessionRoleMismatch);
        }
        Ok(Self {
            endpoint,
            role,
            compatibility,
            session,
            config,
            inbound: FixedQueue::new(config.inbound_capacity),
            ack_queue: FixedQueue::new(MAX_RUNTIME_QUEUE_MESSAGES),
            reliable_outbound: Box::new(std::array::from_fn(|_| None)),
            reliable_terminal: [None; MAX_RUNTIME_QUEUE_MESSAGES],
            unreliable_outbound: std::array::from_fn(|_| None),
            reliable_receive: std::array::from_fn(|_| ReliableReceiveState::new()),
            latest_unreliable_sequence: [None; 5],
            pending_unreliable_message: std::array::from_fn(|_| None),
            next_outbound_sequence: [0; 5],
            recent_unordered_sequences: [None; MAX_RUNTIME_QUEUE_MESSAGES],
            recent_unordered_cursor: 0,
            recent_resync_transfers: Box::new(std::array::from_fn(|_| None)),
            recent_resync_transfer_cursor: 0,
            last_result: None,
            timeout_disconnect_queued: false,
            connection: RuntimeConnectionState::Active,
            metrics: RuntimeMetrics::default(),
            emitted_abuse: RuntimeAbuseSignal::None,
        })
    }

    pub const fn role(&self) -> PeerRole {
        self.role
    }

    pub const fn connection_state(&self) -> RuntimeConnectionState {
        self.connection
    }

    pub const fn metrics(&self) -> &RuntimeMetrics {
        &self.metrics
    }

    pub fn client_session(&self) -> Option<&ClientSession> {
        match &self.session {
            SessionBinding::Client(session) => Some(session),
            _ => None,
        }
    }

    pub fn client_session_mut(&mut self) -> Option<&mut ClientSession> {
        match &mut self.session {
            SessionBinding::Client(session) => Some(session),
            _ => None,
        }
    }

    pub fn authority_gate(&self) -> Option<&AuthoritySessionGate> {
        match &self.session {
            SessionBinding::Authority { gate, .. } => Some(gate),
            _ => None,
        }
    }

    pub fn authority_gate_mut(&mut self) -> Option<&mut AuthoritySessionGate> {
        match &mut self.session {
            SessionBinding::Authority { gate, .. } => Some(gate),
            _ => None,
        }
    }

    pub const fn inbound_len(&self) -> usize {
        self.inbound.len()
    }

    pub fn outbound_len(&self) -> usize {
        self.reliable_outbound.iter().flatten().count()
            + self.unreliable_outbound.iter().flatten().count()
    }

    pub fn reliable_pending_len(&self) -> usize {
        self.reliable_outbound.iter().flatten().count()
    }

    pub fn try_next_event(&mut self) -> Option<RuntimeEvent> {
        self.inbound.pop_front()
    }

    pub fn queue_message(
        &mut self,
        message: WireMessage,
    ) -> Result<QueueDisposition, RuntimeQueueError> {
        self.queue_message_inner(message)
            .map(|(disposition, _)| disposition)
    }

    pub(crate) fn queue_tracked_disconnect(
        &mut self,
        disconnect: DisconnectMessage,
    ) -> Result<ReliableSendHandle, RuntimeQueueError> {
        let (_, handle) = self.queue_message_inner(WireMessage::Disconnect(disconnect))?;
        Ok(handle.expect("Disconnect is carried on a reliable protocol channel"))
    }

    pub(crate) fn reliable_send_status(&self, handle: ReliableSendHandle) -> ReliableSendStatus {
        let index = usize::from(handle.slot);
        let Some(current) = self.reliable_outbound.get(index).and_then(Option::as_ref) else {
            return self
                .reliable_terminal
                .get(index)
                .and_then(|terminal| *terminal)
                .filter(|terminal| {
                    terminal.channel == handle.channel && terminal.sequence == handle.sequence
                })
                .map_or(ReliableSendStatus::Exhausted, |terminal| terminal.status);
        };
        if current.packet.channel != handle.channel || current.packet.sequence != handle.sequence {
            return ReliableSendStatus::Exhausted;
        }
        if current.exhausted {
            ReliableSendStatus::Exhausted
        } else {
            ReliableSendStatus::Pending
        }
    }

    /// Removes non-Control traffic before a terminal Disconnect is queued.
    ///
    /// Existing ordered Control predecessors must remain: removing one would
    /// create a sequence gap, while replacing one that may already have been
    /// delivered would let a duplicate ACK falsely acknowledge the terminal
    /// message. They drain before the Disconnect on the same ordered channel.
    /// Result, Resync, State, and Input traffic cannot leak after Closing.
    pub(crate) fn prepare_for_terminal_disconnect(&mut self) {
        for (index, slot) in self.reliable_outbound.iter_mut().enumerate() {
            if slot
                .as_ref()
                .is_some_and(|pending| pending.packet.channel == AfcChannel::Control)
            {
                continue;
            }
            let Some(pending) = slot.take() else {
                continue;
            };
            self.reliable_terminal[index] = Some(ReliableTerminal {
                channel: pending.packet.channel,
                sequence: pending.packet.sequence,
                status: ReliableSendStatus::Exhausted,
            });
        }
        self.unreliable_outbound.fill(None);
    }

    fn queue_message_inner(
        &mut self,
        message: WireMessage,
    ) -> Result<(QueueDisposition, Option<ReliableSendHandle>), RuntimeQueueError> {
        let channel = protocol_to_afc(message.channel());
        if !message_permits_sender(&message, self.role) {
            self.metrics.direction_rejections = self.metrics.direction_rejections.saturating_add(1);
            return Err(RuntimeQueueError::DirectionDenied {
                role: self.role,
                channel: message.channel(),
            });
        }

        let channel_index = channel_index(channel);
        let replacing_unreliable = matches!(
            channel.metadata().delivery,
            DeliverySemantics::SequencedUnreliable
        ) && self.unreliable_outbound[channel_index].is_some();
        if self.outbound_len() >= self.config.outbound_capacity && !replacing_unreliable {
            self.metrics.outbound_queue_overflows =
                self.metrics.outbound_queue_overflows.saturating_add(1);
            return Err(RuntimeQueueError::OutboundQueueFull);
        }

        let sequence = self.next_outbound_sequence[channel_index];
        let datagram = encode_runtime_message(self.compatibility, channel, sequence, &message)?;
        let packet = OutboundPacket {
            channel,
            sequence,
            datagram,
        };

        let (disposition, handle) = match channel.metadata().delivery {
            DeliverySemantics::SequencedUnreliable => {
                let replaced = self.unreliable_outbound[channel_index]
                    .replace(packet)
                    .is_some();
                if replaced {
                    (QueueDisposition::ReplacedLatest, None)
                } else {
                    (QueueDisposition::Queued, None)
                }
            }
            DeliverySemantics::OrderedReliable | DeliverySemantics::UnorderedReliable => {
                let Some(slot_index) = self.reliable_outbound.iter().position(Option::is_none)
                else {
                    self.metrics.outbound_queue_overflows =
                        self.metrics.outbound_queue_overflows.saturating_add(1);
                    return Err(RuntimeQueueError::OutboundQueueFull);
                };
                self.reliable_outbound[slot_index] = Some(ReliableOutbound {
                    packet,
                    last_sent_tick: None,
                    attempts: 0,
                    exhausted: false,
                });
                self.reliable_terminal[slot_index] = None;
                (
                    QueueDisposition::Queued,
                    Some(ReliableSendHandle {
                        slot: slot_index as u8,
                        channel,
                        sequence,
                    }),
                )
            }
        };
        self.next_outbound_sequence[channel_index] = sequence.wrapping_add(1);
        self.update_queue_high_water();
        Ok((disposition, handle))
    }

    /// Validates that `message` can be queued without mutating sequence numbers,
    /// metrics, or outbound storage. Single-owner orchestrators use this to
    /// preflight an all-peer phase broadcast before committing any one peer.
    pub(crate) fn preflight_message(&self, message: &WireMessage) -> Result<(), RuntimeQueueError> {
        let channel = protocol_to_afc(message.channel());
        if !message_permits_sender(message, self.role) {
            return Err(RuntimeQueueError::DirectionDenied {
                role: self.role,
                channel: message.channel(),
            });
        }
        let channel_index = channel_index(channel);
        let replacing_unreliable = matches!(
            channel.metadata().delivery,
            DeliverySemantics::SequencedUnreliable
        ) && self.unreliable_outbound[channel_index].is_some();
        if self.outbound_len() >= self.config.outbound_capacity && !replacing_unreliable {
            return Err(RuntimeQueueError::OutboundQueueFull);
        }
        let sequence = self.next_outbound_sequence[channel_index];
        encode_runtime_message(self.compatibility, channel, sequence, message)?;
        Ok(())
    }

    pub fn queue_start_message(
        &mut self,
        message: StartMessage,
    ) -> Result<QueueDisposition, RuntimeQueueError> {
        message
            .validate()
            .map_err(RuntimeQueueError::InvalidStartMessage)?;
        self.queue_message(WireMessage::Start(message))
    }

    pub fn abuse_signal(&self) -> RuntimeAbuseSignal {
        if self.metrics.abuse_violations >= self.config.abuse_disconnect_threshold {
            RuntimeAbuseSignal::Disconnect
        } else if self.metrics.abuse_violations >= self.config.abuse_warning_threshold {
            RuntimeAbuseSignal::Warning
        } else {
            RuntimeAbuseSignal::None
        }
    }

    pub fn take_abuse_signal(&mut self) -> RuntimeAbuseSignal {
        let signal = self.abuse_signal();
        if signal > self.emitted_abuse {
            self.emitted_abuse = signal;
            signal
        } else {
            RuntimeAbuseSignal::None
        }
    }

    pub fn pump(&mut self, tick: SimTick) -> PumpReport {
        let events_before = self.inbound.len();
        self.drive_client_session_tick(tick);
        self.drain_ordered_reliable(tick);

        let mut report = PumpReport {
            tick,
            connection: self.connection,
            ..PumpReport::default()
        };
        for _ in 0..self.config.max_receive_datagrams_per_pump {
            match self.endpoint.try_receive() {
                ReceiveOutcome::Received(datagram) => {
                    report.received_datagrams = report.received_datagrams.saturating_add(1);
                    self.metrics.received_datagrams =
                        self.metrics.received_datagrams.saturating_add(1);
                    self.metrics.received_bytes = self
                        .metrics
                        .received_bytes
                        .saturating_add(datagram.len() as u64);
                    self.receive_datagram(datagram, tick);
                }
                ReceiveOutcome::Empty => break,
                ReceiveOutcome::Disconnected => {
                    self.mark_transport_disconnected();
                    break;
                }
                ReceiveOutcome::Oversized { .. } => {
                    self.metrics.malformed_datagrams =
                        self.metrics.malformed_datagrams.saturating_add(1);
                    self.note_violation(2);
                }
                ReceiveOutcome::IoError(kind) => {
                    self.record_transport_error(kind);
                    break;
                }
            }
        }
        if usize::from(report.received_datagrams) == self.config.max_receive_datagrams_per_pump {
            self.metrics.receive_budget_exhaustions =
                self.metrics.receive_budget_exhaustions.saturating_add(1);
            self.note_violation(1);
        }

        self.drain_ordered_reliable(tick);
        self.drain_latest_unreliable(tick);
        report.sent_datagrams = self.flush_outbound(tick);
        report.queued_events = self.inbound.len().saturating_sub(events_before) as u16;
        report.connection = self.connection;
        report.abuse = self.abuse_signal();
        self.update_queue_high_water();
        report
    }

    fn drive_client_session_tick(&mut self, tick: SimTick) {
        let mut timeout = None;
        let mut session_error = None;
        if let SessionBinding::Client(session) = &mut self.session {
            if let Err(error) = session.observe_tick(tick) {
                session_error = Some(error);
            }
            if !self.timeout_disconnect_queued {
                timeout = session.check_timeout(tick);
            }
        }
        if let Some(error) = session_error {
            self.push_session_error(error);
        }
        if let Some(disconnect) = timeout {
            if self
                .queue_message(WireMessage::Disconnect(disconnect))
                .is_ok()
            {
                self.timeout_disconnect_queued = true;
            }
        }
    }

    fn receive_datagram(&mut self, datagram: AfcDatagram, tick: SimTick) {
        let envelope = match decode_envelope(datagram.as_slice()) {
            Ok(envelope) => envelope,
            Err(_) => {
                self.metrics.malformed_datagrams =
                    self.metrics.malformed_datagrams.saturating_add(1);
                self.note_violation(1);
                return;
            }
        };

        if let Some(acknowledged) = envelope.acknowledged_sequence {
            self.accept_ack(envelope.channel, acknowledged);
            return;
        }
        let Some(sequence) = envelope.message_sequence else {
            self.metrics.malformed_datagrams = self.metrics.malformed_datagrams.saturating_add(1);
            self.note_violation(1);
            return;
        };

        let remote_role = self.role.remote();
        if !envelope
            .channel
            .metadata()
            .permits_sender(remote_role.endpoint_role())
        {
            self.metrics.direction_rejections = self.metrics.direction_rejections.saturating_add(1);
            self.note_violation(2);
            return;
        }
        let decoded = match decode_packet(envelope.payload, &self.compatibility) {
            Ok(decoded) => decoded,
            Err(_) => {
                self.metrics.decode_rejections = self.metrics.decode_rejections.saturating_add(1);
                self.note_violation(1);
                return;
            }
        };
        if protocol_to_afc(decoded.header.channel) != envelope.channel {
            self.metrics.malformed_datagrams = self.metrics.malformed_datagrams.saturating_add(1);
            self.note_violation(2);
            return;
        }
        if !message_permits_sender(&decoded.message, remote_role) {
            self.metrics.direction_rejections = self.metrics.direction_rejections.saturating_add(1);
            self.note_violation(2);
            return;
        }
        self.metrics.decoded_messages = self.metrics.decoded_messages.saturating_add(1);

        match envelope.channel.metadata().delivery {
            DeliverySemantics::SequencedUnreliable => {
                self.accept_unreliable(envelope.channel, sequence, decoded.message);
            }
            DeliverySemantics::OrderedReliable => {
                self.accept_ordered_reliable(envelope.channel, sequence, decoded.message);
            }
            DeliverySemantics::UnorderedReliable => {
                self.accept_unordered_reliable(envelope.channel, sequence, decoded.message, tick);
            }
        }
    }

    fn accept_unreliable(&mut self, channel: AfcChannel, sequence: u32, message: WireMessage) {
        let index = channel_index(channel);
        if self.latest_unreliable_sequence[index]
            .is_some_and(|latest| !sequence_is_newer(sequence, latest))
        {
            self.metrics.stale_or_duplicate_unreliable =
                self.metrics.stale_or_duplicate_unreliable.saturating_add(1);
            return;
        }
        self.latest_unreliable_sequence[index] = Some(sequence);
        self.pending_unreliable_message[index] = Some(message);
    }

    fn accept_ordered_reliable(
        &mut self,
        channel: AfcChannel,
        sequence: u32,
        message: WireMessage,
    ) {
        let index = channel_index(channel);
        match self.reliable_receive[index].insert(
            sequence,
            message,
            self.config.reliable_reorder_capacity,
        ) {
            ReliableInsert::Inserted => {
                self.metrics.reliable_high_water = self
                    .metrics
                    .reliable_high_water
                    .max(self.reliable_receive[index].len);
            }
            ReliableInsert::AlreadyDelivered => {
                self.metrics.duplicate_reliable = self.metrics.duplicate_reliable.saturating_add(1);
                self.queue_ack(channel, sequence);
            }
            ReliableInsert::DuplicateBuffered => {
                self.metrics.duplicate_reliable = self.metrics.duplicate_reliable.saturating_add(1);
            }
            ReliableInsert::TooFarAhead => {
                self.metrics.reliable_reorder_overflows =
                    self.metrics.reliable_reorder_overflows.saturating_add(1);
                self.note_violation(1);
            }
        }
    }

    fn accept_unordered_reliable(
        &mut self,
        channel: AfcChannel,
        sequence: u32,
        message: WireMessage,
        tick: SimTick,
    ) {
        if self
            .recent_unordered_sequences
            .iter()
            .flatten()
            .any(|seen| *seen == sequence)
        {
            self.metrics.duplicate_reliable = self.metrics.duplicate_reliable.saturating_add(1);
            self.queue_ack(channel, sequence);
            return;
        }
        match self.dispatch_message(message, tick) {
            DispatchOutcome::Blocked => {
                self.metrics.inbound_queue_overflows =
                    self.metrics.inbound_queue_overflows.saturating_add(1);
            }
            DispatchOutcome::Delivered | DispatchOutcome::Consumed => {
                self.recent_unordered_sequences[self.recent_unordered_cursor] = Some(sequence);
                self.recent_unordered_cursor =
                    (self.recent_unordered_cursor + 1) % MAX_RUNTIME_QUEUE_MESSAGES;
                self.queue_ack(channel, sequence);
            }
        }
    }

    fn drain_ordered_reliable(&mut self, tick: SimTick) {
        for channel in [AfcChannel::Control, AfcChannel::Result] {
            let index = channel_index(channel);
            loop {
                let defer_reconnect_result =
                    self.reliable_receive[index].next().is_some_and(|pending| {
                        matches!(pending.message, WireMessage::ResultIdentifier(_))
                            && self
                                .client_session()
                                .is_some_and(ClientSession::is_reconnect_initial_sync)
                    });
                if defer_reconnect_result {
                    // Result and Control are independent reliable channels. A
                    // reconnect's Result may therefore overtake the final clock
                    // reply that permits InitialSync -> Fighting. Leave Result
                    // unconsumed and unacknowledged until that exact reconnect
                    // gate completes; ordinary early Results still fail closed
                    // in dispatch_message below.
                    break;
                }
                let Some(message) = self.reliable_receive[index].take_next() else {
                    break;
                };
                let sequence = message.sequence;
                match self.dispatch_message(message.message.clone(), tick) {
                    DispatchOutcome::Blocked => {
                        self.reliable_receive[index].restore_next(message);
                        self.metrics.inbound_queue_overflows =
                            self.metrics.inbound_queue_overflows.saturating_add(1);
                        break;
                    }
                    DispatchOutcome::Delivered | DispatchOutcome::Consumed => {
                        self.queue_ack(channel, sequence);
                    }
                }
            }
        }
    }

    fn drain_latest_unreliable(&mut self, tick: SimTick) {
        for channel in [AfcChannel::Input, AfcChannel::State] {
            let index = channel_index(channel);
            let Some(message) = self.pending_unreliable_message[index].take() else {
                continue;
            };
            if self.dispatch_message(message.clone(), tick) == DispatchOutcome::Blocked {
                self.pending_unreliable_message[index] = Some(message);
                self.metrics.inbound_queue_overflows =
                    self.metrics.inbound_queue_overflows.saturating_add(1);
            }
        }
    }

    fn dispatch_message(&mut self, message: WireMessage, tick: SimTick) -> DispatchOutcome {
        if let WireMessage::ResultIdentifier(result) = &message {
            if let Some(previous) = self.last_result {
                if previous.result_id == result.result_id {
                    if previous == *result {
                        self.metrics.duplicate_results =
                            self.metrics.duplicate_results.saturating_add(1);
                    } else {
                        self.metrics.conflicting_idempotent_messages = self
                            .metrics
                            .conflicting_idempotent_messages
                            .saturating_add(1);
                        self.note_violation(2);
                    }
                    return DispatchOutcome::Consumed;
                }
            }
        }
        if let WireMessage::ResyncChunk(chunk) = &message {
            match self.classify_resync_chunk(chunk) {
                ResyncClassification::Duplicate => {
                    self.metrics.duplicate_resync_chunks =
                        self.metrics.duplicate_resync_chunks.saturating_add(1);
                    return DispatchOutcome::Consumed;
                }
                ResyncClassification::Conflict => {
                    self.metrics.conflicting_idempotent_messages = self
                        .metrics
                        .conflicting_idempotent_messages
                        .saturating_add(1);
                    self.note_violation(2);
                    return DispatchOutcome::Consumed;
                }
                ResyncClassification::New(_) => {}
            }
        }
        if self.inbound.is_full() {
            return DispatchOutcome::Blocked;
        }

        if let WireMessage::Handshake(Handshake { compatibility }) = &message {
            let session_error = match &mut self.session {
                SessionBinding::Authority { gate, remote_peer } => {
                    gate.authenticate(*remote_peer, *compatibility).err()
                }
                _ => None,
            };
            if let Some(error) = session_error {
                self.push_session_error(error);
                self.note_violation(2);
                return DispatchOutcome::Consumed;
            }
            return self.push_message_event(message);
        }

        if let WireMessage::Start(start) = &message {
            return self.dispatch_start_message(*start, message, tick);
        }

        if let WireMessage::ClockProbe(probe) = &message {
            if matches!(self.session, SessionBinding::None) {
                // Production authority orchestrators may intentionally own their
                // multi-peer readiness state above the one-endpoint runtime.
                return self.push_message_event(message);
            }
            let SessionBinding::Authority { gate, remote_peer } = &self.session else {
                self.note_violation(2);
                return DispatchOutcome::Consumed;
            };
            let expected_match = gate.match_id();
            if let Err(error) = validate_authority_start_identity(
                expected_match,
                *remote_peer,
                probe.match_id,
                probe.peer_id,
            ) {
                self.push_session_error(error);
                self.note_violation(2);
                return DispatchOutcome::Consumed;
            }
            if self.outbound_len() >= self.config.outbound_capacity {
                return DispatchOutcome::Blocked;
            }

            let mut preview = *gate;
            if let Err(error) = preview.observe_clock_probe(*remote_peer, probe.probe_id) {
                self.push_session_error(error);
                self.note_violation(2);
                return DispatchOutcome::Consumed;
            }
            let reply = WireMessage::ClockReply(ClockReply {
                match_id: probe.match_id,
                peer_id: *remote_peer,
                probe_id: probe.probe_id,
                authority_tick: tick,
            });
            if self.queue_message(reply).is_err() {
                return DispatchOutcome::Blocked;
            }
            let SessionBinding::Authority { gate, .. } = &mut self.session else {
                unreachable!("authority session binding was checked above")
            };
            *gate = preview;
            return self.push_message_event(message);
        }

        if let WireMessage::ResultIdentifier(result) = &message {
            let session_error = match &mut self.session {
                SessionBinding::Client(session) => {
                    let confirmed = ConfirmedSessionResult {
                        result_id: result.result_id.get(),
                        final_tick: result.final_tick,
                        final_hash: result.final_state_hash,
                    };
                    let mut preview = *session;
                    let result = (|| {
                        // A confirmed result is itself the authoritative wire
                        // signal that fighting ended. A client whose render/network
                        // pump was paused may still be in Countdown when the packet
                        // is drained, so observe the current tick before entering
                        // confirmation. Earlier phases still fail closed.
                        preview.observe_tick(tick)?;
                        if preview.phase() == crate::network_protocol::ConnectionPhase::Fighting {
                            preview.begin_result_confirmation(tick)?;
                        }
                        preview.accept_confirmed_result(result.match_id, confirmed, tick)
                    })();
                    match result {
                        Ok(()) => {
                            *session = preview;
                            None
                        }
                        Err(error) => Some(error),
                    }
                }
                _ => None,
            };
            if let Some(error) = session_error {
                self.push_session_error(error);
                self.note_violation(1);
                return DispatchOutcome::Consumed;
            }
            self.last_result = Some(*result);
            return self.push_message_event(message);
        }

        if let WireMessage::ResyncChunk(chunk) = &message {
            let ResyncClassification::New(recent) = self.classify_resync_chunk(chunk) else {
                unreachable!("resync classification was checked before queue capacity")
            };
            let outcome = self.push_message_event(message);
            if outcome == DispatchOutcome::Delivered {
                self.record_resync_chunk(recent);
            }
            return outcome;
        }

        if matches!(message, WireMessage::Disconnect(_)) {
            let outcome = self.push_message_event(message);
            if outcome == DispatchOutcome::Delivered {
                self.connection = RuntimeConnectionState::RemoteDisconnect;
            }
            return outcome;
        }
        self.push_message_event(message)
    }

    fn dispatch_start_message(
        &mut self,
        start: StartMessage,
        event: WireMessage,
        tick: SimTick,
    ) -> DispatchOutcome {
        let outbound_has_space = self.outbound_len() < self.config.outbound_capacity;
        let mut automatic_response = None;
        let mut session_error = None;
        let mut client_before = None;
        let mut authority_before = None;

        match &mut self.session {
            SessionBinding::Client(session) => match start {
                StartMessage::Manifest(manifest) => {
                    let before = *session;
                    let mut preview = before;
                    match preview.accept_manifest(manifest, tick) {
                        Ok(response) if outbound_has_space => {
                            client_before = Some(before);
                            *session = preview;
                            automatic_response = Some(response);
                        }
                        Ok(_) => return DispatchOutcome::Blocked,
                        Err(error) => session_error = Some(error),
                    }
                }
                StartMessage::Countdown { .. } => {
                    let mut preview = *session;
                    match preview.begin_countdown(start, tick) {
                        Ok(()) => *session = preview,
                        Err(error) => session_error = Some(error),
                    }
                }
                _ => session_error = Some(SessionError::PeerMismatch),
            },
            SessionBinding::Authority { gate, remote_peer } => {
                let before = *gate;
                let mut preview = before;
                let expected_match = preview.match_id();
                let result = match start {
                    StartMessage::ManifestAccepted {
                        match_id,
                        peer_id,
                        manifest_hash,
                    } => validate_authority_start_identity(
                        expected_match,
                        *remote_peer,
                        match_id,
                        peer_id,
                    )
                    .and_then(|()| preview.accept_manifest(*remote_peer, manifest_hash)),
                    StartMessage::InitialSyncApplied {
                        match_id,
                        peer_id,
                        snapshot_tick,
                        snapshot_hash,
                    } => validate_authority_start_identity(
                        expected_match,
                        *remote_peer,
                        match_id,
                        peer_id,
                    )
                    .and_then(|()| {
                        preview.apply_initial_sync(
                            *remote_peer,
                            AppliedInitialSync {
                                tick: snapshot_tick,
                                hash: snapshot_hash,
                            },
                        )
                    }),
                    StartMessage::Ready { match_id, peer_id } => validate_authority_start_identity(
                        expected_match,
                        *remote_peer,
                        match_id,
                        peer_id,
                    )
                    .and_then(|()| preview.mark_ready(*remote_peer)),
                    _ => Err(SessionError::PeerMismatch),
                };
                match result {
                    Ok(()) => {
                        if matches!(start, StartMessage::Ready { .. }) && preview.all_ready() {
                            if !outbound_has_space {
                                return DispatchOutcome::Blocked;
                            }
                            match preview.begin_countdown(tick, DEFAULT_COUNTDOWN_LEAD_TICKS) {
                                Ok(countdown) => automatic_response = Some(countdown),
                                Err(error) => session_error = Some(error),
                            }
                        }
                        if session_error.is_none() {
                            authority_before = Some(before);
                            *gate = preview;
                        }
                    }
                    Err(error) => session_error = Some(error),
                }
            }
            SessionBinding::None => {}
        }

        if let Some(error) = session_error {
            self.push_session_error(error);
            self.note_violation(2);
            return DispatchOutcome::Consumed;
        }
        if let Some(response) = automatic_response {
            if self.queue_start_message(response).is_err() {
                if let (SessionBinding::Client(session), Some(before)) =
                    (&mut self.session, client_before)
                {
                    *session = before;
                }
                if let (SessionBinding::Authority { gate, .. }, Some(before)) =
                    (&mut self.session, authority_before)
                {
                    *gate = before;
                }
                return DispatchOutcome::Blocked;
            }
        }
        self.push_message_event(event)
    }

    fn push_message_event(&mut self, message: WireMessage) -> DispatchOutcome {
        if self
            .inbound
            .push_back(RuntimeEvent::Message(message))
            .is_err()
        {
            DispatchOutcome::Blocked
        } else {
            self.metrics.delivered_messages = self.metrics.delivered_messages.saturating_add(1);
            self.metrics.inbound_high_water =
                self.metrics.inbound_high_water.max(self.inbound.len());
            DispatchOutcome::Delivered
        }
    }

    fn push_session_error(&mut self, error: SessionError) {
        if self
            .inbound
            .push_back(RuntimeEvent::SessionError(error))
            .is_err()
        {
            self.metrics.inbound_queue_overflows =
                self.metrics.inbound_queue_overflows.saturating_add(1);
        } else {
            self.metrics.inbound_high_water =
                self.metrics.inbound_high_water.max(self.inbound.len());
        }
    }

    fn classify_resync_chunk(&self, chunk: &ResyncChunk) -> ResyncClassification {
        let recent = RecentResyncChunk {
            match_id: chunk.match_id,
            transfer_id: chunk.transfer_id,
            chunk_index: chunk.chunk_index,
            chunk_count: chunk.chunk_count,
            fingerprint: resync_fingerprint(chunk),
        };
        let matching = self.recent_resync_transfers.iter().flatten().find(|entry| {
            entry.match_id == recent.match_id && entry.transfer_id == recent.transfer_id
        });
        match matching {
            Some(existing) if existing.chunk_count != recent.chunk_count => {
                ResyncClassification::Conflict
            }
            Some(existing)
                if existing.fingerprint(recent.chunk_index) == Some(recent.fingerprint) =>
            {
                ResyncClassification::Duplicate
            }
            Some(existing) if existing.fingerprint(recent.chunk_index).is_some() => {
                ResyncClassification::Conflict
            }
            Some(_) | None => ResyncClassification::New(recent),
        }
    }

    fn record_resync_chunk(&mut self, chunk: RecentResyncChunk) {
        if let Some(existing) = self
            .recent_resync_transfers
            .iter_mut()
            .flatten()
            .find(|entry| {
                entry.match_id == chunk.match_id && entry.transfer_id == chunk.transfer_id
            })
        {
            existing.record(chunk);
            return;
        }
        self.recent_resync_transfers[self.recent_resync_transfer_cursor] =
            Some(RecentResyncTransfer::new(chunk));
        self.recent_resync_transfer_cursor =
            (self.recent_resync_transfer_cursor + 1) % MAX_RECENT_RESYNC_TRANSFERS;
    }

    fn queue_ack(&mut self, channel: AfcChannel, sequence: u32) {
        let ack = PendingAck { channel, sequence };
        if self.ack_queue.any(|existing| *existing == ack) {
            return;
        }
        if self.ack_queue.push_back(ack).is_err() {
            self.metrics.ack_queue_overflows = self.metrics.ack_queue_overflows.saturating_add(1);
            self.note_violation(1);
        }
    }

    fn accept_ack(&mut self, channel: AfcChannel, sequence: u32) {
        if matches!(
            channel.metadata().delivery,
            DeliverySemantics::SequencedUnreliable
        ) {
            self.metrics.malformed_datagrams = self.metrics.malformed_datagrams.saturating_add(1);
            self.note_violation(1);
            return;
        }
        if let Some(index) = self.reliable_outbound.iter().position(|slot| {
            slot.as_ref().is_some_and(|pending| {
                pending.packet.channel == channel && pending.packet.sequence == sequence
            })
        }) {
            self.reliable_outbound[index] = None;
            self.reliable_terminal[index] = Some(ReliableTerminal {
                channel,
                sequence,
                status: ReliableSendStatus::Acknowledged,
            });
            self.metrics.reliable_acks_received =
                self.metrics.reliable_acks_received.saturating_add(1);
        } else {
            self.metrics.duplicate_reliable = self.metrics.duplicate_reliable.saturating_add(1);
        }
    }

    fn flush_outbound(&mut self, tick: SimTick) -> u16 {
        let mut sent = 0u16;
        let budget = self.config.max_send_datagrams_per_pump;

        while usize::from(sent) < budget {
            let Some(ack) = self.ack_queue.pop_front() else {
                break;
            };
            let datagram = encode_ack(ack.channel, ack.sequence);
            match self.endpoint.try_send(datagram) {
                SendOutcome::Sent => {
                    sent = sent.saturating_add(1);
                    self.metrics.sent_datagrams = self.metrics.sent_datagrams.saturating_add(1);
                    self.metrics.sent_bytes = self
                        .metrics
                        .sent_bytes
                        .saturating_add(RUNTIME_ENVELOPE_BYTES as u64);
                    self.metrics.reliable_acks_sent =
                        self.metrics.reliable_acks_sent.saturating_add(1);
                }
                SendOutcome::Full(_) => {
                    let _ = self.ack_queue.push_front(ack);
                    self.metrics.send_would_block = self.metrics.send_would_block.saturating_add(1);
                    return sent;
                }
                SendOutcome::Disconnected(_) => {
                    self.mark_transport_disconnected();
                    return sent;
                }
                SendOutcome::IoError { kind, .. } => {
                    self.record_transport_error(kind);
                    return sent;
                }
            }
        }

        for index in 0..MAX_RUNTIME_QUEUE_MESSAGES {
            if usize::from(sent) >= budget {
                break;
            }
            let Some(pending) = &self.reliable_outbound[index] else {
                continue;
            };
            if pending.exhausted || !reliable_send_due(pending, tick, self.config) {
                continue;
            }
            if pending.attempts >= self.config.reliable_max_attempts {
                self.reliable_outbound[index]
                    .as_mut()
                    .expect("reliable pending was just observed")
                    .exhausted = true;
                self.metrics.retry_exhaustions = self.metrics.retry_exhaustions.saturating_add(1);
                self.connection = RuntimeConnectionState::RetryExhausted;
                continue;
            }
            let datagram = pending.packet.datagram.clone();
            let bytes = datagram.len();
            let was_retry = pending.attempts > 0;
            match self.endpoint.try_send(datagram) {
                SendOutcome::Sent => {
                    let pending = self.reliable_outbound[index]
                        .as_mut()
                        .expect("sent reliable packet remains pending until acknowledged");
                    pending.last_sent_tick = Some(tick);
                    pending.attempts = pending.attempts.saturating_add(1);
                    sent = sent.saturating_add(1);
                    self.metrics.sent_datagrams = self.metrics.sent_datagrams.saturating_add(1);
                    self.metrics.sent_bytes = self.metrics.sent_bytes.saturating_add(bytes as u64);
                    if was_retry {
                        self.metrics.reliable_retries =
                            self.metrics.reliable_retries.saturating_add(1);
                    }
                }
                SendOutcome::Full(_) => {
                    self.metrics.send_would_block = self.metrics.send_would_block.saturating_add(1);
                    return sent;
                }
                SendOutcome::Disconnected(_) => {
                    self.mark_transport_disconnected();
                    return sent;
                }
                SendOutcome::IoError { kind, .. } => {
                    self.record_transport_error(kind);
                    return sent;
                }
            }
        }

        for channel in AfcChannel::ALL {
            if usize::from(sent) >= budget {
                break;
            }
            let index = channel_index(channel);
            let Some(packet) = self.unreliable_outbound[index].take() else {
                continue;
            };
            let bytes = packet.datagram.len();
            match self.endpoint.try_send(packet.datagram.clone()) {
                SendOutcome::Sent => {
                    sent = sent.saturating_add(1);
                    self.metrics.sent_datagrams = self.metrics.sent_datagrams.saturating_add(1);
                    self.metrics.sent_bytes = self.metrics.sent_bytes.saturating_add(bytes as u64);
                }
                SendOutcome::Full(_) => {
                    self.unreliable_outbound[index] = Some(packet);
                    self.metrics.send_would_block = self.metrics.send_would_block.saturating_add(1);
                    return sent;
                }
                SendOutcome::Disconnected(_) => {
                    self.unreliable_outbound[index] = Some(packet);
                    self.mark_transport_disconnected();
                    return sent;
                }
                SendOutcome::IoError { kind, .. } => {
                    self.unreliable_outbound[index] = Some(packet);
                    self.record_transport_error(kind);
                    return sent;
                }
            }
        }

        if usize::from(sent) >= budget
            && (!self.ack_queue.is_empty()
                || self.reliable_outbound.iter().flatten().any(|pending| {
                    !pending.exhausted && reliable_send_due(pending, tick, self.config)
                })
                || self.unreliable_outbound.iter().any(Option::is_some))
        {
            self.metrics.send_budget_exhaustions =
                self.metrics.send_budget_exhaustions.saturating_add(1);
        }
        sent
    }

    fn mark_transport_disconnected(&mut self) {
        if self.connection == RuntimeConnectionState::TransportDisconnected {
            return;
        }
        self.connection = RuntimeConnectionState::TransportDisconnected;
        if self
            .inbound
            .push_back(RuntimeEvent::TransportDisconnected)
            .is_err()
        {
            self.metrics.inbound_queue_overflows =
                self.metrics.inbound_queue_overflows.saturating_add(1);
        }
    }

    fn record_transport_error(&mut self, kind: io::ErrorKind) {
        self.metrics.transport_errors = self.metrics.transport_errors.saturating_add(1);
        if matches!(
            kind,
            io::ErrorKind::ConnectionAborted
                | io::ErrorKind::ConnectionRefused
                | io::ErrorKind::ConnectionReset
                | io::ErrorKind::NotConnected
                | io::ErrorKind::BrokenPipe
        ) {
            self.mark_transport_disconnected();
        }
    }

    fn note_violation(&mut self, amount: u32) {
        self.metrics.abuse_violations = self.metrics.abuse_violations.saturating_add(amount);
    }

    fn update_queue_high_water(&mut self) {
        self.metrics.inbound_high_water = self.metrics.inbound_high_water.max(self.inbound.len());
        self.metrics.outbound_high_water =
            self.metrics.outbound_high_water.max(self.outbound_len());
        self.metrics.reliable_high_water = self
            .metrics
            .reliable_high_water
            .max(self.reliable_receive.iter().map(|state| state.len).sum());
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResyncClassification {
    New(RecentResyncChunk),
    Duplicate,
    Conflict,
}

fn channel_index(channel: AfcChannel) -> usize {
    usize::from(channel.wire_id() - 1)
}

fn protocol_to_afc(channel: ProtocolChannel) -> AfcChannel {
    match channel {
        ProtocolChannel::Control => AfcChannel::Control,
        ProtocolChannel::Input => AfcChannel::Input,
        ProtocolChannel::State => AfcChannel::State,
        ProtocolChannel::Resync => AfcChannel::Resync,
        ProtocolChannel::Result => AfcChannel::Result,
    }
}

fn message_permits_sender(message: &WireMessage, role: PeerRole) -> bool {
    match message {
        WireMessage::InputBatch(_) => role == PeerRole::Client,
        WireMessage::CommittedInputRelay(_) => role == PeerRole::Authority,
        WireMessage::Start(StartMessage::Manifest(_) | StartMessage::Countdown { .. }) => {
            role == PeerRole::Authority
        }
        WireMessage::Start(
            StartMessage::ManifestAccepted { .. }
            | StartMessage::InitialSyncApplied { .. }
            | StartMessage::Ready { .. },
        ) => role == PeerRole::Client,
        WireMessage::ResyncRequest(_) | WireMessage::ResyncApplied(_) => role == PeerRole::Client,
        WireMessage::ResyncBegin(_) | WireMessage::ResyncInputTail(_) => {
            role == PeerRole::Authority
        }
        WireMessage::ClockProbe(_) => role == PeerRole::Client,
        WireMessage::ClockReply(_) => role == PeerRole::Authority,
        _ => protocol_to_afc(message.channel())
            .metadata()
            .permits_sender(role.endpoint_role()),
    }
}

fn validate_authority_start_identity(
    expected_match: MatchId,
    expected_peer: PeerId,
    received_match: MatchId,
    received_peer: PeerId,
) -> Result<(), SessionError> {
    if received_match != expected_match {
        return Err(SessionError::ManifestMismatch);
    }
    if received_peer != expected_peer {
        return Err(SessionError::PeerMismatch);
    }
    Ok(())
}

fn sequence_is_newer(candidate: u32, current: u32) -> bool {
    let distance = candidate.wrapping_sub(current);
    distance != 0 && distance < (1u32 << 31)
}

fn reliable_send_due(pending: &ReliableOutbound, tick: SimTick, config: RuntimeConfig) -> bool {
    pending.last_sent_tick.is_none_or(|last| {
        tick.0.wrapping_sub(last.0) >= u64::from(config.reliable_retry_interval_ticks)
    })
}

fn encode_runtime_message(
    compatibility: CompatibilityId,
    channel: AfcChannel,
    sequence: u32,
    message: &WireMessage,
) -> Result<AfcDatagram, RuntimeQueueError> {
    let mut inner = [0u8; crate::network_codec::MAX_PACKET_BYTES];
    let inner_len = encode_packet(compatibility.protocol, message, &mut inner)?;
    if RUNTIME_ENVELOPE_BYTES + inner_len > MAX_AFC_DATAGRAM_BYTES {
        return Err(RuntimeQueueError::DatagramTooLarge);
    }
    let mut bytes = [0u8; MAX_AFC_DATAGRAM_BYTES];
    write_envelope_header(&mut bytes, channel, FLAG_MESSAGE, sequence, 0, inner_len);
    bytes[RUNTIME_ENVELOPE_BYTES..RUNTIME_ENVELOPE_BYTES + inner_len]
        .copy_from_slice(&inner[..inner_len]);
    AfcDatagram::try_from_slice(&bytes[..RUNTIME_ENVELOPE_BYTES + inner_len])
        .map_err(|_| RuntimeQueueError::DatagramTooLarge)
}

fn encode_ack(channel: AfcChannel, acknowledged_sequence: u32) -> AfcDatagram {
    let mut bytes = [0u8; RUNTIME_ENVELOPE_BYTES];
    write_envelope_header(&mut bytes, channel, FLAG_ACK, 0, acknowledged_sequence, 0);
    AfcDatagram::try_from_slice(&bytes).expect("fixed runtime ACK envelope fits datagram ceiling")
}

fn write_envelope_header(
    output: &mut [u8],
    channel: AfcChannel,
    flags: u8,
    sequence: u32,
    acknowledged_sequence: u32,
    payload_len: usize,
) {
    output[..4].copy_from_slice(&RUNTIME_MAGIC);
    output[4] = RUNTIME_ENVELOPE_VERSION;
    output[5] = channel.wire_id();
    output[6] = flags;
    output[7] = 0;
    output[8..12].copy_from_slice(&sequence.to_be_bytes());
    output[12..16].copy_from_slice(&acknowledged_sequence.to_be_bytes());
    output[16..18].copy_from_slice(&(payload_len as u16).to_be_bytes());
}

fn decode_envelope(packet: &[u8]) -> Result<DecodedEnvelope<'_>, EnvelopeError> {
    if packet.len() < RUNTIME_ENVELOPE_BYTES {
        return Err(EnvelopeError::TooShort);
    }
    if packet[..4] != RUNTIME_MAGIC {
        return Err(EnvelopeError::BadMagic);
    }
    if packet[4] != RUNTIME_ENVELOPE_VERSION {
        return Err(EnvelopeError::BadVersion);
    }
    let channel = AfcChannel::try_from(packet[5]).map_err(|_| EnvelopeError::UnknownChannel)?;
    let flags = packet[6];
    if flags != FLAG_MESSAGE && flags != FLAG_ACK {
        return Err(EnvelopeError::InvalidFlags);
    }
    if packet[7] != 0 {
        return Err(EnvelopeError::NonZeroReserved);
    }
    let sequence = u32::from_be_bytes(packet[8..12].try_into().expect("fixed header slice"));
    let acknowledged = u32::from_be_bytes(packet[12..16].try_into().expect("fixed header slice"));
    let payload_len = usize::from(u16::from_be_bytes([packet[16], packet[17]]));
    if packet.len() != RUNTIME_ENVELOPE_BYTES + payload_len {
        return Err(EnvelopeError::LengthMismatch);
    }
    if flags == FLAG_ACK && payload_len != 0 {
        return Err(EnvelopeError::InvalidFlags);
    }
    if flags == FLAG_MESSAGE && payload_len == 0 {
        return Err(EnvelopeError::InvalidFlags);
    }
    Ok(DecodedEnvelope {
        channel,
        message_sequence: (flags == FLAG_MESSAGE).then_some(sequence),
        acknowledged_sequence: (flags == FLAG_ACK).then_some(acknowledged),
        payload: &packet[RUNTIME_ENVELOPE_BYTES..],
    })
}

fn resync_fingerprint(chunk: &ResyncChunk) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x100_0000_01b3;
    let mut bytes = [0u8; crate::network_protocol::MAX_RESYNC_CHUNK_BYTES];
    let len = chunk
        .payload
        .copy_prefix_into(chunk.payload_len, &mut bytes)
        .expect("decoded resync chunks have validated payload padding");
    let mut hash = FNV_OFFSET;
    for byte in chunk
        .snapshot_tick
        .0
        .to_be_bytes()
        .into_iter()
        .chain(chunk.snapshot_hash.0.to_be_bytes())
        .chain(chunk.chunk_count.to_be_bytes())
        .chain(chunk.payload_len.to_be_bytes())
        .chain(bytes[..len].iter().copied())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network_codec::{ProcessedInputAck, ResultId, StateHashAndAcks};
    use crate::network_io::{
        DeterministicNetworkLab, DisconnectWindow, FaultConfig, FaultLabConfig, FaultLabEndpoint,
        InProcessEndpoint, PROBABILITY_SCALE,
    };
    use crate::network_protocol::{
        AuthorityKind, BuildId, ClockProbe, ClockProbeId, CommittedInputRecord,
        CommittedInputRelay, CommittedInputSource, CommittedSeatInputWindow, ConnectionPhase,
        DefinitionId, DisconnectCode, FighterId, FighterSlotConfig, GameplayContentHash,
        InputBatch, InputButtons, InputFrame, InputSequence, MAX_FIGHTERS, ManifestHash, MatchId,
        MatchManifest, ProtocolVersion, QuantizedAxis, ReplayFormatVersion, ResyncChunkPayload,
        ResyncInputTail, RetryDisposition, SIMULATION_HZ, SeatAssignment, SeatId, SeatInputWindow,
        SeatOwner, SeatOwnership, SimulationVersion, StateHash, TeamId, TransferId,
    };
    use crate::session::SessionTimeouts;

    fn compatibility() -> CompatibilityId {
        CompatibilityId {
            protocol: ProtocolVersion::new(1).unwrap(),
            simulation: SimulationVersion::new(1).unwrap(),
            replay: ReplayFormatVersion::new(1).unwrap(),
            build: BuildId::new([1; 16]).unwrap(),
            gameplay_content: GameplayContentHash::new([2; 32]).unwrap(),
        }
    }

    fn match_id() -> MatchId {
        MatchId::new([3; 16]).unwrap()
    }

    fn peer_id() -> PeerId {
        PeerId::new(7).unwrap()
    }

    fn manifest() -> MatchManifest {
        let ownership = SeatOwnership::from_assignments(&[
            SeatAssignment {
                seat: SeatId::new(0).unwrap(),
                fighter: FighterId::new(0).unwrap(),
                owner: SeatOwner::Peer(peer_id()),
            },
            SeatAssignment {
                seat: SeatId::new(1).unwrap(),
                fighter: FighterId::new(1).unwrap(),
                owner: SeatOwner::Peer(peer_id()),
            },
        ])
        .unwrap();
        let mut slots = [FighterSlotConfig::default(); MAX_FIGHTERS];
        for (index, slot) in slots.iter_mut().take(2).enumerate() {
            *slot = FighterSlotConfig {
                occupied: true,
                fighter: FighterId::new(index as u8).unwrap(),
                team: TeamId::new(index as u8).unwrap(),
                character: DefinitionId::new(index as u16).unwrap(),
                style: DefinitionId::new(0).unwrap(),
                equipment: DefinitionId::new(0).unwrap(),
            };
        }
        MatchManifest {
            compatibility: compatibility(),
            manifest_hash: ManifestHash(9),
            match_id: match_id(),
            authority: AuthorityKind::Listen,
            trusted_results: false,
            arena: DefinitionId::new(0).unwrap(),
            rules: DefinitionId::new(0).unwrap(),
            slots,
            ownership,
            master_gameplay_seed: 42,
            rng_scheme_version: 1,
            tick_rate_hz: SIMULATION_HZ,
            input_delay_ticks: 2,
            rollback_limit_ticks: 12,
            snapshot_history_ticks: 32,
            agreed_start_tick: SimTick(100),
        }
    }

    fn runtime_pair(
        config: RuntimeConfig,
    ) -> (
        NetworkRuntime<InProcessEndpoint>,
        NetworkRuntime<InProcessEndpoint>,
    ) {
        let (client_endpoint, authority_endpoint) = InProcessEndpoint::pair(128).unwrap();
        (
            NetworkRuntime::new(client_endpoint, PeerRole::Client, compatibility(), config)
                .unwrap(),
            NetworkRuntime::new(
                authority_endpoint,
                PeerRole::Authority,
                compatibility(),
                config,
            )
            .unwrap(),
        )
    }

    fn pump_pair(
        client: &mut NetworkRuntime<InProcessEndpoint>,
        authority: &mut NetworkRuntime<InProcessEndpoint>,
        tick: u64,
    ) {
        client.pump(SimTick(tick));
        authority.pump(SimTick(tick));
        client.pump(SimTick(tick));
        authority.pump(SimTick(tick));
    }

    fn input_frame(tick: u64, seat: u8, sequence: u16) -> InputFrame {
        InputFrame {
            tick: SimTick(tick),
            seat: SeatId::new(seat).unwrap(),
            movement_x: QuantizedAxis::new(30).unwrap(),
            movement_y: QuantizedAxis::new(-20).unwrap(),
            held_buttons: InputButtons::new(InputButtons::LIGHT).unwrap(),
            pressed_buttons: InputButtons::new(InputButtons::LIGHT).unwrap(),
            released_buttons: InputButtons::default(),
            sequence: InputSequence(sequence),
        }
    }

    fn disconnect_message() -> DisconnectMessage {
        DisconnectMessage {
            match_id: Some(match_id()),
            code: DisconnectCode::Kicked,
            retry: RetryDisposition::ReturnToLobby,
            detail_code: 17,
            last_confirmed_tick: Some(SimTick(9)),
        }
    }

    fn input_batch(tick: u64) -> InputBatch {
        let window =
            SeatInputWindow::from_newest_first(&[input_frame(tick, 0, tick as u16)]).unwrap();
        InputBatch::new(match_id(), peer_id(), &[window]).unwrap()
    }

    fn committed_relay(tick: u64) -> CommittedInputRelay {
        let window = CommittedSeatInputWindow::from_newest_first(&[CommittedInputRecord {
            frame: input_frame(tick, 0, tick as u16),
            fighter: FighterId::ZERO,
            source: CommittedInputSource::Peer(peer_id()),
        }])
        .unwrap();
        CommittedInputRelay::new(match_id(), SimTick(tick), &[window]).unwrap()
    }

    fn fault_runtime_pair(
        config: FaultConfig,
        seed: u64,
        runtime_config: RuntimeConfig,
    ) -> (
        DeterministicNetworkLab,
        NetworkRuntime<FaultLabEndpoint>,
        NetworkRuntime<FaultLabEndpoint>,
    ) {
        let (lab, client_endpoint, authority_endpoint) =
            DeterministicNetworkLab::pair(FaultLabConfig::symmetric(config, seed)).unwrap();
        (
            lab,
            NetworkRuntime::new(
                client_endpoint,
                PeerRole::Client,
                compatibility(),
                runtime_config,
            )
            .unwrap(),
            NetworkRuntime::new(
                authority_endpoint,
                PeerRole::Authority,
                compatibility(),
                runtime_config,
            )
            .unwrap(),
        )
    }

    fn resync_chunk(index: u16, bytes: &[u8]) -> ResyncChunk {
        resync_chunk_for(5, index, 2, bytes)
    }

    fn resync_chunk_for(
        transfer_id: u32,
        index: u16,
        chunk_count: u16,
        bytes: &[u8],
    ) -> ResyncChunk {
        let (payload, payload_len) = ResyncChunkPayload::from_bytes(bytes).unwrap();
        ResyncChunk {
            match_id: match_id(),
            transfer_id: TransferId::new(transfer_id).unwrap(),
            snapshot_tick: SimTick(40),
            snapshot_hash: StateHash(88),
            chunk_index: index,
            chunk_count,
            payload_len,
            payload,
        }
    }

    fn record_new_resync_chunk(
        runtime: &mut NetworkRuntime<InProcessEndpoint>,
        chunk: &ResyncChunk,
    ) {
        let ResyncClassification::New(recent) = runtime.classify_resync_chunk(chunk) else {
            panic!("test chunk was not new: {chunk:?}");
        };
        runtime.record_resync_chunk(recent);
    }

    #[test]
    fn startup_messages_drive_client_and_authority_sessions_end_to_end() {
        let (client_endpoint, authority_endpoint) = InProcessEndpoint::pair(16).unwrap();
        let gate = AuthoritySessionGate::new(manifest()).unwrap();
        let mut client_session =
            ClientSession::new(compatibility(), SessionTimeouts::default(), SimTick(0)).unwrap();
        client_session.enter_lobby(SimTick(0)).unwrap();
        client_session.start_connecting(SimTick(1)).unwrap();
        client_session.transport_connected(SimTick(2)).unwrap();
        client_session
            .authentication_succeeded(peer_id(), SimTick(3))
            .unwrap();
        let mut client = NetworkRuntime::new_client(
            client_endpoint,
            compatibility(),
            client_session,
            RuntimeConfig::default(),
        )
        .unwrap();
        let mut authority = NetworkRuntime::new_authority(
            authority_endpoint,
            compatibility(),
            gate,
            peer_id(),
            RuntimeConfig::default(),
        )
        .unwrap();

        client
            .queue_message(WireMessage::Handshake(Handshake {
                compatibility: compatibility(),
            }))
            .unwrap();
        authority
            .queue_start_message(StartMessage::Manifest(manifest()))
            .unwrap();
        pump_pair(&mut client, &mut authority, 4);

        assert!(
            authority
                .authority_gate()
                .unwrap()
                .peer(peer_id())
                .unwrap()
                .authenticated
        );
        assert_eq!(
            client.client_session().unwrap().phase(),
            ConnectionPhase::Loading
        );
        assert_eq!(
            authority
                .authority_gate()
                .unwrap()
                .peer(peer_id())
                .unwrap()
                .manifest_hash,
            Some(manifest().manifest_hash)
        );

        let initial_sync_applied = {
            let session = client.client_session_mut().unwrap();
            session.content_loaded(SimTick(5)).unwrap();
            session
                .apply_initial_sync(match_id(), SimTick(50), StateHash(8), SimTick(6))
                .unwrap()
        };
        client.queue_start_message(initial_sync_applied).unwrap();
        for probe_id in 1..=3 {
            client
                .queue_message(WireMessage::ClockProbe(ClockProbe {
                    match_id: match_id(),
                    peer_id: peer_id(),
                    probe_id: ClockProbeId::new(probe_id).unwrap(),
                }))
                .unwrap();
        }
        pump_pair(&mut client, &mut authority, 8);
        client
            .client_session_mut()
            .unwrap()
            .mark_clock_synchronized()
            .unwrap();
        let ready = client.client_session().unwrap().ready_message().unwrap();
        client.queue_start_message(ready).unwrap();
        pump_pair(&mut client, &mut authority, 9);

        let readiness = authority.authority_gate().unwrap().peer(peer_id()).unwrap();
        assert_eq!(
            readiness.initial_sync,
            Some(AppliedInitialSync {
                tick: SimTick(50),
                hash: StateHash(8),
            })
        );
        assert!(readiness.ready);
        assert_eq!(
            client.client_session().unwrap().phase(),
            ConnectionPhase::Countdown
        );
        let actual_start = authority
            .authority_gate()
            .unwrap()
            .countdown_start_tick()
            .unwrap();
        assert_eq!(actual_start, SimTick(129));
        assert_eq!(
            client.client_session().unwrap().countdown_start_tick(),
            Some(actual_start)
        );
        client.pump(actual_start);
        assert_eq!(
            client.client_session().unwrap().phase(),
            ConnectionPhase::Fighting
        );
    }

    #[test]
    fn startup_message_directions_are_enforced_before_queueing() {
        let (mut client, mut authority) = runtime_pair(RuntimeConfig::default());
        assert!(matches!(
            client.queue_start_message(StartMessage::Manifest(manifest())),
            Err(RuntimeQueueError::DirectionDenied {
                role: PeerRole::Client,
                channel: ProtocolChannel::Control,
            })
        ));
        assert!(matches!(
            authority.queue_start_message(StartMessage::Ready {
                match_id: match_id(),
                peer_id: peer_id(),
            }),
            Err(RuntimeQueueError::DirectionDenied {
                role: PeerRole::Authority,
                channel: ProtocolChannel::Control,
            })
        ));
        let request = crate::network_protocol::ResyncRequest {
            match_id: match_id(),
            peer_id: peer_id(),
            reason: crate::network_protocol::ResyncReason::InitialSync,
            last_confirmed_tick: SimTick::ZERO,
            last_confirmed_hash: StateHash(0),
        };
        assert!(
            client
                .queue_message(WireMessage::ResyncRequest(request))
                .is_ok()
        );
        assert!(matches!(
            authority.queue_message(WireMessage::ResyncRequest(request)),
            Err(RuntimeQueueError::DirectionDenied { .. })
        ));

        let begin = crate::network_protocol::ResyncBegin {
            match_id: match_id(),
            transfer_id: crate::network_protocol::TransferId::new(1).unwrap(),
            snapshot_tick: SimTick(1),
            snapshot_hash: StateHash(2),
            snapshot_bytes: 1,
            chunk_count: 1,
            recent_input_start: SimTick(1),
            recent_input_end: SimTick(1),
        };
        assert!(
            authority
                .queue_message(WireMessage::ResyncBegin(begin))
                .is_ok()
        );
        assert!(matches!(
            client.queue_message(WireMessage::ResyncBegin(begin)),
            Err(RuntimeQueueError::DirectionDenied { .. })
        ));

        let committed_window =
            CommittedSeatInputWindow::from_newest_first(&[CommittedInputRecord {
                frame: input_frame(1, 0, 1),
                fighter: FighterId::ZERO,
                source: CommittedInputSource::Peer(peer_id()),
            }])
            .unwrap();
        let relay = CommittedInputRelay::new(match_id(), SimTick(1), &[committed_window]).unwrap();
        let input_tail = ResyncInputTail::new(&begin, &[committed_window]).unwrap();
        assert!(
            authority
                .queue_message(WireMessage::ResyncInputTail(input_tail))
                .is_ok()
        );
        assert!(matches!(
            client.queue_message(WireMessage::ResyncInputTail(input_tail)),
            Err(RuntimeQueueError::DirectionDenied { .. })
        ));
        assert!(matches!(
            client.queue_message(WireMessage::CommittedInputRelay(relay)),
            Err(RuntimeQueueError::DirectionDenied { .. })
        ));
        assert!(
            authority
                .queue_message(WireMessage::CommittedInputRelay(relay))
                .is_ok()
        );

        let input = input_batch(1);
        assert!(matches!(
            authority.queue_message(WireMessage::InputBatch(input)),
            Err(RuntimeQueueError::DirectionDenied { .. })
        ));
    }

    #[test]
    fn multiple_local_seat_input_batch_crosses_transport_once() {
        let (mut client, mut authority) = runtime_pair(RuntimeConfig::default());
        let windows = [
            SeatInputWindow::from_newest_first(&[input_frame(10, 0, 20)]).unwrap(),
            SeatInputWindow::from_newest_first(&[input_frame(10, 1, 40)]).unwrap(),
        ];
        let batch = InputBatch::new(match_id(), peer_id(), &windows).unwrap();
        client
            .queue_message(WireMessage::InputBatch(batch))
            .unwrap();
        pump_pair(&mut client, &mut authority, 10);

        let Some(RuntimeEvent::Message(WireMessage::InputBatch(received))) =
            authority.try_next_event()
        else {
            panic!("authority did not receive the input batch");
        };
        assert_eq!(received.len(), 2);
        assert_eq!(
            received.as_slice()[0].newest().unwrap().seat,
            SeatId::new(0).unwrap()
        );
        assert_eq!(
            received.as_slice()[1].newest().unwrap().seat,
            SeatId::new(1).unwrap()
        );
    }

    #[test]
    fn committed_input_relay_is_authority_only_and_latest_wins() {
        let (mut client, mut authority) = runtime_pair(RuntimeConfig::default());
        let first = committed_relay(20);
        let latest = committed_relay(21);

        assert!(matches!(
            client.queue_message(WireMessage::CommittedInputRelay(first)),
            Err(RuntimeQueueError::DirectionDenied { .. })
        ));
        authority
            .queue_message(WireMessage::CommittedInputRelay(first))
            .unwrap();
        assert_eq!(
            authority
                .queue_message(WireMessage::CommittedInputRelay(latest))
                .unwrap(),
            QueueDisposition::ReplacedLatest
        );
        pump_pair(&mut client, &mut authority, 21);
        let Some(RuntimeEvent::Message(WireMessage::CommittedInputRelay(received))) =
            client.try_next_event()
        else {
            panic!("client did not receive committed inputs");
        };
        assert_eq!(received.authority_tick, SimTick(21));
    }

    #[test]
    fn state_channel_is_authority_only_and_latest_hash_wins() {
        let (mut client, mut authority) = runtime_pair(RuntimeConfig::default());
        let first = StateHashAndAcks::new(
            match_id(),
            SimTick(20),
            StateHash(1),
            &[ProcessedInputAck {
                seat: SeatId::new(0).unwrap(),
                processed_through: SimTick(19),
                sequence: InputSequence(19),
            }],
        )
        .unwrap();
        let latest = StateHashAndAcks::new(
            match_id(),
            SimTick(21),
            StateHash(2),
            &[ProcessedInputAck {
                seat: SeatId::new(0).unwrap(),
                processed_through: SimTick(20),
                sequence: InputSequence(20),
            }],
        )
        .unwrap();

        assert!(matches!(
            client.queue_message(WireMessage::StateHashAndAcks(first)),
            Err(RuntimeQueueError::DirectionDenied { .. })
        ));
        authority
            .queue_message(WireMessage::StateHashAndAcks(first))
            .unwrap();
        assert_eq!(
            authority
                .queue_message(WireMessage::StateHashAndAcks(latest))
                .unwrap(),
            QueueDisposition::ReplacedLatest
        );
        pump_pair(&mut client, &mut authority, 21);
        let Some(RuntimeEvent::Message(WireMessage::StateHashAndAcks(received))) =
            client.try_next_event()
        else {
            panic!("client did not receive state");
        };
        assert_eq!(received.authority_tick, SimTick(21));
        assert_eq!(received.state_hash, StateHash(2));
    }

    #[test]
    fn reliable_resync_chunks_arrive_and_are_acknowledged() {
        let (mut client, mut authority) = runtime_pair(RuntimeConfig::default());
        authority
            .queue_message(WireMessage::ResyncChunk(resync_chunk(0, &[1, 2, 3])))
            .unwrap();
        authority
            .queue_message(WireMessage::ResyncChunk(resync_chunk(1, &[4, 5])))
            .unwrap();

        for tick in 1..=3 {
            pump_pair(&mut client, &mut authority, tick);
        }
        let mut indices = [u16::MAX; 2];
        for output in &mut indices {
            let Some(RuntimeEvent::Message(WireMessage::ResyncChunk(chunk))) =
                client.try_next_event()
            else {
                panic!("missing resync chunk");
            };
            *output = chunk.chunk_index;
        }
        indices.sort_unstable();
        assert_eq!(indices, [0, 1]);
        assert_eq!(authority.reliable_pending_len(), 0);
        assert_eq!(authority.metrics().reliable_acks_received, 2);
    }

    #[test]
    fn full_128_chunk_transfer_retains_first_chunk_fingerprint() {
        let (mut client, _authority) = runtime_pair(RuntimeConfig::default());
        for index in 0..MAX_RESYNC_CHUNKS as u16 {
            record_new_resync_chunk(
                &mut client,
                &resync_chunk_for(5, index, MAX_RESYNC_CHUNKS as u16, &[index as u8]),
            );
        }

        assert_eq!(
            client.classify_resync_chunk(&resync_chunk_for(5, 0, MAX_RESYNC_CHUNKS as u16, &[0],)),
            ResyncClassification::Duplicate
        );
        assert_eq!(
            client.classify_resync_chunk(&resync_chunk_for(
                5,
                MAX_RESYNC_CHUNKS as u16 - 1,
                MAX_RESYNC_CHUNKS as u16,
                &[127],
            )),
            ResyncClassification::Duplicate
        );
        assert_eq!(
            client.classify_resync_chunk(&resync_chunk_for(
                5,
                0,
                MAX_RESYNC_CHUNKS as u16,
                &[0xFF],
            )),
            ResyncClassification::Conflict
        );
    }

    #[test]
    fn active_and_previous_resync_transfers_have_independent_fingerprints() {
        let (mut client, _authority) = runtime_pair(RuntimeConfig::default());
        for transfer_id in [5, 6] {
            for index in 0..MAX_RESYNC_CHUNKS as u16 {
                record_new_resync_chunk(
                    &mut client,
                    &resync_chunk_for(
                        transfer_id,
                        index,
                        MAX_RESYNC_CHUNKS as u16,
                        &[transfer_id as u8, index as u8],
                    ),
                );
            }
        }

        for transfer_id in [5, 6] {
            assert_eq!(
                client.classify_resync_chunk(&resync_chunk_for(
                    transfer_id,
                    0,
                    MAX_RESYNC_CHUNKS as u16,
                    &[transfer_id as u8, 0],
                )),
                ResyncClassification::Duplicate
            );
        }
        assert_eq!(
            client.classify_resync_chunk(&resync_chunk_for(
                6,
                0,
                MAX_RESYNC_CHUNKS as u16,
                &[0xEE],
            )),
            ResyncClassification::Conflict
        );

        record_new_resync_chunk(
            &mut client,
            &resync_chunk_for(7, 0, MAX_RESYNC_CHUNKS as u16, &[7, 0]),
        );
        assert!(matches!(
            client.classify_resync_chunk(&resync_chunk_for(
                5,
                0,
                MAX_RESYNC_CHUNKS as u16,
                &[5, 0],
            )),
            ResyncClassification::New(_)
        ));
        assert_eq!(
            client.classify_resync_chunk(&resync_chunk_for(
                6,
                0,
                MAX_RESYNC_CHUNKS as u16,
                &[6, 0],
            )),
            ResyncClassification::Duplicate
        );
        assert_eq!(
            client.classify_resync_chunk(&resync_chunk_for(
                7,
                0,
                MAX_RESYNC_CHUNKS as u16,
                &[7, 0],
            )),
            ResyncClassification::Duplicate
        );
    }

    #[test]
    fn result_identifier_is_semantically_idempotent_across_new_packet_sequences() {
        let (mut client, mut authority) = runtime_pair(RuntimeConfig::default());
        let result = ResultIdentifier {
            match_id: match_id(),
            result_id: ResultId::new(99).unwrap(),
            final_tick: SimTick(400),
            final_state_hash: StateHash(123),
        };
        authority
            .queue_message(WireMessage::ResultIdentifier(result))
            .unwrap();
        pump_pair(&mut client, &mut authority, 1);
        assert!(matches!(
            client.try_next_event(),
            Some(RuntimeEvent::Message(WireMessage::ResultIdentifier(_)))
        ));

        authority
            .queue_message(WireMessage::ResultIdentifier(result))
            .unwrap();
        pump_pair(&mut client, &mut authority, 2);
        assert!(client.try_next_event().is_none());
        assert_eq!(client.metrics().duplicate_results, 1);
    }

    #[test]
    fn malformed_traffic_escalates_and_receive_work_is_bounded() {
        let (mut raw, endpoint) = InProcessEndpoint::pair(32).unwrap();
        let config = RuntimeConfig {
            max_receive_datagrams_per_pump: 4,
            abuse_warning_threshold: 2,
            abuse_disconnect_threshold: 5,
            ..RuntimeConfig::default()
        };
        let mut runtime =
            NetworkRuntime::new(endpoint, PeerRole::Authority, compatibility(), config).unwrap();
        for _ in 0..8 {
            assert!(matches!(
                raw.try_send(AfcDatagram::try_from_slice(b"bad").unwrap()),
                SendOutcome::Sent
            ));
        }

        let first = runtime.pump(SimTick(1));
        assert_eq!(first.received_datagrams, 4);
        assert_eq!(runtime.abuse_signal(), RuntimeAbuseSignal::Disconnect);
        assert_eq!(runtime.metrics().receive_budget_exhaustions, 1);
        assert_eq!(runtime.take_abuse_signal(), RuntimeAbuseSignal::Disconnect);
        assert_eq!(runtime.take_abuse_signal(), RuntimeAbuseSignal::None);
    }

    #[test]
    fn bounded_backpressure_preserves_reliable_message_until_space_opens() {
        let config = RuntimeConfig {
            inbound_capacity: 1,
            outbound_capacity: 1,
            reliable_reorder_capacity: 2,
            ..RuntimeConfig::default()
        };
        let (mut client, mut authority) = runtime_pair(config);
        let handshake = WireMessage::Handshake(Handshake {
            compatibility: compatibility(),
        });
        client.queue_message(handshake.clone()).unwrap();
        assert_eq!(
            client.queue_message(handshake.clone()),
            Err(RuntimeQueueError::OutboundQueueFull)
        );
        client.pump(SimTick(1));
        authority.pump(SimTick(1));
        assert_eq!(authority.inbound_len(), 1);
        assert!(authority.inbound_len() <= config.inbound_capacity);
        assert!(client.outbound_len() <= config.outbound_capacity);

        assert!(authority.try_next_event().is_some());
        authority.pump(SimTick(2));
        client.pump(SimTick(2));
        assert_eq!(client.reliable_pending_len(), 0);
    }

    #[test]
    fn tracked_disconnect_reports_only_its_exact_acknowledgement() {
        let (mut client, mut authority) = runtime_pair(RuntimeConfig::default());
        let handle = authority
            .queue_tracked_disconnect(disconnect_message())
            .unwrap();
        assert_eq!(
            authority.reliable_send_status(handle),
            ReliableSendStatus::Pending
        );

        authority.receive_datagram(
            encode_ack(handle.channel, handle.sequence.wrapping_add(1)),
            SimTick(1),
        );
        assert_eq!(
            authority.reliable_send_status(handle),
            ReliableSendStatus::Pending
        );

        authority.pump(SimTick(1));
        client.pump(SimTick(1));
        authority.pump(SimTick(2));
        assert_eq!(
            authority.reliable_send_status(handle),
            ReliableSendStatus::Acknowledged
        );

        authority.receive_datagram(encode_ack(handle.channel, handle.sequence), SimTick(2));
        assert_eq!(
            authority.reliable_send_status(handle),
            ReliableSendStatus::Acknowledged
        );
        assert_eq!(authority.metrics().reliable_acks_received, 1);
        assert!(authority.metrics().duplicate_reliable >= 2);
    }

    #[test]
    fn terminal_disconnect_preserves_sent_control_order_and_purges_result() {
        let config = RuntimeConfig {
            max_send_datagrams_per_pump: 1,
            ..RuntimeConfig::default()
        };
        let (mut client, mut authority) = runtime_pair(config);
        authority
            .queue_start_message(StartMessage::Manifest(manifest()))
            .unwrap();
        authority
            .queue_message(WireMessage::ResultIdentifier(ResultIdentifier {
                match_id: match_id(),
                result_id: ResultId::new(5).unwrap(),
                final_tick: SimTick(9),
                final_state_hash: StateHash(10),
            }))
            .unwrap();

        // The ordered Control predecessor has left the runtime but its ACK is
        // still absent. Result remains queued because the send budget was one.
        authority.pump(SimTick(1));
        authority.prepare_for_terminal_disconnect();
        let handle = authority
            .queue_tracked_disconnect(disconnect_message())
            .unwrap();

        for tick in 1..=5 {
            client.pump(SimTick(tick));
            authority.pump(SimTick(tick));
        }
        assert_eq!(
            authority.reliable_send_status(handle),
            ReliableSendStatus::Acknowledged
        );
        let mut saw_manifest = false;
        let mut saw_disconnect = false;
        while let Some(event) = client.try_next_event() {
            match event {
                RuntimeEvent::Message(WireMessage::Start(StartMessage::Manifest(_))) => {
                    saw_manifest = true;
                }
                RuntimeEvent::Message(WireMessage::Disconnect(_)) => saw_disconnect = true,
                RuntimeEvent::Message(WireMessage::ResultIdentifier(_)) => {
                    panic!("Result leaked after terminal disconnect preparation");
                }
                _ => {}
            }
        }
        assert!(saw_manifest);
        assert!(saw_disconnect);
    }

    #[test]
    fn tracked_disconnect_reports_retry_exhaustion_when_ack_is_lost() {
        let config = RuntimeConfig {
            reliable_retry_interval_ticks: 1,
            reliable_max_attempts: 2,
            ..RuntimeConfig::default()
        };
        let (_client, mut authority) = runtime_pair(config);
        let handle = authority
            .queue_tracked_disconnect(disconnect_message())
            .unwrap();

        authority.pump(SimTick(1));
        authority.pump(SimTick(2));
        authority.pump(SimTick(3));

        assert_eq!(
            authority.reliable_send_status(handle),
            ReliableSendStatus::Exhausted
        );
        assert_eq!(
            authority.connection_state(),
            RuntimeConnectionState::RetryExhausted
        );
        assert_eq!(authority.metrics().retry_exhaustions, 1);
    }

    #[test]
    fn blocked_disconnect_stays_buffered_unacknowledged_and_active() {
        let config = RuntimeConfig {
            inbound_capacity: 1,
            reliable_reorder_capacity: 2,
            ..RuntimeConfig::default()
        };
        let (mut client, mut authority) = runtime_pair(config);
        client
            .queue_message(WireMessage::Handshake(Handshake {
                compatibility: compatibility(),
            }))
            .unwrap();
        let handle = client
            .queue_tracked_disconnect(disconnect_message())
            .unwrap();

        client.pump(SimTick(1));
        authority.pump(SimTick(1));
        client.pump(SimTick(1));
        assert_eq!(authority.connection_state(), RuntimeConnectionState::Active);
        assert_eq!(
            client.reliable_send_status(handle),
            ReliableSendStatus::Pending
        );
        assert!(matches!(
            authority.try_next_event(),
            Some(RuntimeEvent::Message(WireMessage::Handshake(_)))
        ));

        authority.pump(SimTick(2));
        client.pump(SimTick(2));
        assert_eq!(
            authority.connection_state(),
            RuntimeConnectionState::RemoteDisconnect
        );
        assert_eq!(
            client.reliable_send_status(handle),
            ReliableSendStatus::Acknowledged
        );
        assert!(matches!(
            authority.try_next_event(),
            Some(RuntimeEvent::Message(WireMessage::Disconnect(_)))
        ));
    }

    #[test]
    fn client_session_timeout_can_queue_typed_disconnect_without_transport_leakage() {
        let (client_endpoint, _authority_endpoint) = InProcessEndpoint::pair(8).unwrap();
        let mut session =
            ClientSession::new(compatibility(), SessionTimeouts::default(), SimTick(0)).unwrap();
        session.enter_lobby(SimTick(0)).unwrap();
        session.start_connecting(SimTick(1)).unwrap();
        let deadline = session.deadline_tick().unwrap();
        let mut runtime = NetworkRuntime::new_client(
            client_endpoint,
            compatibility(),
            session,
            RuntimeConfig::default(),
        )
        .unwrap();
        runtime.pump(deadline);
        assert_eq!(runtime.reliable_pending_len(), 1);
    }

    #[test]
    fn envelope_rejects_noncanonical_flags_and_lengths() {
        let ack = encode_ack(AfcChannel::Control, 9);
        assert!(decode_envelope(ack.as_slice()).is_ok());

        let mut bytes = ack.as_slice().to_vec();
        bytes[6] = FLAG_MESSAGE | FLAG_ACK;
        assert!(matches!(
            decode_envelope(&bytes),
            Err(EnvelopeError::InvalidFlags)
        ));
        bytes[6] = FLAG_ACK;
        bytes.push(0);
        assert!(matches!(
            decode_envelope(&bytes),
            Err(EnvelopeError::LengthMismatch)
        ));
    }

    #[test]
    fn client_binding_accepts_confirmed_result_when_session_is_in_confirmation() {
        let (client_endpoint, authority_endpoint) = InProcessEndpoint::pair(16).unwrap();
        let mut session =
            ClientSession::new(compatibility(), SessionTimeouts::default(), SimTick(0)).unwrap();
        session.enter_lobby(SimTick(0)).unwrap();
        session.start_connecting(SimTick(1)).unwrap();
        session.transport_connected(SimTick(2)).unwrap();
        session
            .authentication_succeeded(peer_id(), SimTick(3))
            .unwrap();
        session.accept_manifest(manifest(), SimTick(4)).unwrap();
        session.content_loaded(SimTick(5)).unwrap();
        session
            .apply_initial_sync(match_id(), SimTick(50), StateHash(8), SimTick(6))
            .unwrap();
        session.mark_clock_synchronized().unwrap();
        session
            .begin_countdown(
                StartMessage::Countdown {
                    match_id: match_id(),
                    start_tick: SimTick(100),
                },
                SimTick(7),
            )
            .unwrap();
        session.observe_tick(SimTick(100)).unwrap();
        session.begin_result_confirmation(SimTick(200)).unwrap();

        let mut client = NetworkRuntime::new_client(
            client_endpoint,
            compatibility(),
            session,
            RuntimeConfig::default(),
        )
        .unwrap();
        let mut authority = NetworkRuntime::new(
            authority_endpoint,
            PeerRole::Authority,
            compatibility(),
            RuntimeConfig::default(),
        )
        .unwrap();
        authority
            .queue_message(WireMessage::ResultIdentifier(ResultIdentifier {
                match_id: match_id(),
                result_id: ResultId::new(77).unwrap(),
                final_tick: SimTick(199),
                final_state_hash: StateHash(55),
            }))
            .unwrap();
        pump_pair(&mut client, &mut authority, 201);
        assert_eq!(
            client.client_session().unwrap().result().unwrap().result_id,
            77
        );
    }

    #[test]
    fn reconnect_result_waits_unacked_until_reconnect_enters_fighting() {
        let (client_endpoint, authority_endpoint) = InProcessEndpoint::pair(16).unwrap();
        let mut session = ClientSession::new_reconnect(
            compatibility(),
            SessionTimeouts::default(),
            peer_id(),
            manifest(),
            SimTick(100),
            SimTick(200),
        )
        .unwrap();
        let mut client = NetworkRuntime::new_client(
            client_endpoint,
            compatibility(),
            session,
            RuntimeConfig::default(),
        )
        .unwrap();
        let mut authority = NetworkRuntime::new(
            authority_endpoint,
            PeerRole::Authority,
            compatibility(),
            RuntimeConfig::default(),
        )
        .unwrap();
        let result = ResultIdentifier {
            match_id: match_id(),
            result_id: ResultId::new(78).unwrap(),
            final_tick: SimTick(199),
            final_state_hash: StateHash(56),
        };
        authority
            .queue_message(WireMessage::ResultIdentifier(result))
            .unwrap();

        authority.pump(SimTick(201));
        client.pump(SimTick(201));
        authority.pump(SimTick(201));
        assert!(client.try_next_event().is_none());
        assert_eq!(
            client.client_session().unwrap().phase(),
            ConnectionPhase::InitialSync
        );
        assert_eq!(authority.reliable_pending_len(), 1);

        session = *client.client_session().unwrap();
        session.mark_clock_synchronized().unwrap();
        session
            .complete_reconnect(
                AppliedInitialSync {
                    tick: SimTick(198),
                    hash: StateHash(55),
                },
                SimTick(202),
            )
            .unwrap();
        *client.client_session_mut().unwrap() = session;

        client.pump(SimTick(202));
        authority.pump(SimTick(202));
        assert!(matches!(
            client.try_next_event(),
            Some(RuntimeEvent::Message(WireMessage::ResultIdentifier(candidate)))
                if candidate == result
        ));
        assert_eq!(
            client.client_session().unwrap().phase(),
            ConnectionPhase::Results
        );
        assert_eq!(authority.reliable_pending_len(), 0);
    }

    #[test]
    fn ordinary_initial_sync_result_still_fails_closed() {
        let (client_endpoint, authority_endpoint) = InProcessEndpoint::pair(16).unwrap();
        let mut session =
            ClientSession::new(compatibility(), SessionTimeouts::default(), SimTick(0)).unwrap();
        session.enter_lobby(SimTick(0)).unwrap();
        session.start_connecting(SimTick(1)).unwrap();
        session.transport_connected(SimTick(2)).unwrap();
        session
            .authentication_succeeded(peer_id(), SimTick(3))
            .unwrap();
        session.accept_manifest(manifest(), SimTick(4)).unwrap();
        session.content_loaded(SimTick(5)).unwrap();
        assert_eq!(session.phase(), ConnectionPhase::InitialSync);
        assert!(!session.is_reconnect_initial_sync());

        let mut client = NetworkRuntime::new_client(
            client_endpoint,
            compatibility(),
            session,
            RuntimeConfig::default(),
        )
        .unwrap();
        let mut authority = NetworkRuntime::new(
            authority_endpoint,
            PeerRole::Authority,
            compatibility(),
            RuntimeConfig::default(),
        )
        .unwrap();
        authority
            .queue_message(WireMessage::ResultIdentifier(ResultIdentifier {
                match_id: match_id(),
                result_id: ResultId::new(79).unwrap(),
                final_tick: SimTick(6),
                final_state_hash: StateHash(57),
            }))
            .unwrap();

        pump_pair(&mut client, &mut authority, 6);
        assert!(matches!(
            client.try_next_event(),
            Some(RuntimeEvent::SessionError(SessionError::ResultBeforeFight))
        ));
        assert!(client.client_session().unwrap().result().is_none());
        assert_eq!(authority.reliable_pending_len(), 0);
    }

    #[test]
    fn runtime_latest_wins_when_input_datagrams_are_reordered() {
        let faults = FaultConfig {
            base_latency_ticks: 1,
            reorder_per_10k: PROBABILITY_SCALE,
            max_reorder_extra_ticks: 8,
            queue_capacity_packets: 64,
            ..FaultConfig::default()
        };
        let (lab, mut client, mut authority) =
            fault_runtime_pair(faults, 0xAFC0_0DDE, RuntimeConfig::default());
        let mut received_ticks = Vec::new();

        for tick in 0..40 {
            lab.advance_to(tick).unwrap();
            client
                .queue_message(WireMessage::InputBatch(input_batch(tick)))
                .unwrap();
            client.pump(SimTick(tick));
            authority.pump(SimTick(tick));
            while let Some(RuntimeEvent::Message(WireMessage::InputBatch(batch))) =
                authority.try_next_event()
            {
                received_ticks.push(batch.as_slice()[0].newest().unwrap().tick.0);
            }
        }
        for tick in 40..=60 {
            lab.advance_to(tick).unwrap();
            client.pump(SimTick(tick));
            authority.pump(SimTick(tick));
            while let Some(RuntimeEvent::Message(WireMessage::InputBatch(batch))) =
                authority.try_next_event()
            {
                received_ticks.push(batch.as_slice()[0].newest().unwrap().tick.0);
            }
        }

        assert_eq!(received_ticks.last(), Some(&39));
        assert!(received_ticks.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(authority.metrics().stale_or_duplicate_unreliable > 0);
        assert_eq!(lab.metrics().a_to_b.pending_datagrams, 0);
    }

    #[test]
    fn runtime_reliable_channels_recover_under_degraded_faults() {
        let faults = FaultConfig {
            base_latency_ticks: 4,
            jitter_ticks: 2,
            loss_per_10k: 4_000,
            duplication_per_10k: 2_000,
            reorder_per_10k: 3_000,
            max_reorder_extra_ticks: 6,
            delivery_burst_interval_ticks: 2,
            queue_capacity_packets: 64,
            ..FaultConfig::default()
        };
        let runtime_config = RuntimeConfig {
            reliable_retry_interval_ticks: 2,
            reliable_max_attempts: 128,
            ..RuntimeConfig::default()
        };
        let (lab, mut client, mut authority) =
            fault_runtime_pair(faults, 0xDE6A_ADED, runtime_config);
        client
            .queue_message(WireMessage::Handshake(Handshake {
                compatibility: compatibility(),
            }))
            .unwrap();
        authority
            .queue_message(WireMessage::ResultIdentifier(ResultIdentifier {
                match_id: match_id(),
                result_id: ResultId::new(555).unwrap(),
                final_tick: SimTick(900),
                final_state_hash: StateHash(0xCAFE),
            }))
            .unwrap();

        let mut received_handshake = 0;
        let mut received_result = 0;
        for tick in 0..600 {
            lab.advance_to(tick).unwrap();
            client.pump(SimTick(tick));
            authority.pump(SimTick(tick));
            client.pump(SimTick(tick));
            authority.pump(SimTick(tick));
            while let Some(event) = client.try_next_event() {
                if matches!(
                    event,
                    RuntimeEvent::Message(WireMessage::ResultIdentifier(_))
                ) {
                    received_result += 1;
                }
            }
            while let Some(event) = authority.try_next_event() {
                if matches!(event, RuntimeEvent::Message(WireMessage::Handshake(_))) {
                    received_handshake += 1;
                }
            }
            if received_handshake == 1
                && received_result == 1
                && client.reliable_pending_len() == 0
                && authority.reliable_pending_len() == 0
            {
                break;
            }
        }

        assert_eq!(received_handshake, 1);
        assert_eq!(received_result, 1);
        assert_eq!(client.reliable_pending_len(), 0);
        assert_eq!(authority.reliable_pending_len(), 0);
        assert_eq!(client.connection_state(), RuntimeConnectionState::Active);
        assert_eq!(authority.connection_state(), RuntimeConnectionState::Active);
        let metrics = lab.metrics();
        assert!(metrics.a_to_b.dropped_by_loss + metrics.b_to_a.dropped_by_loss > 0);
        assert!(client.metrics().reliable_retries + authority.metrics().reliable_retries > 0);
        assert!(metrics.a_to_b.pending_high_water <= faults.queue_capacity_packets);
        assert!(metrics.b_to_a.pending_high_water <= faults.queue_capacity_packets);
    }

    #[test]
    fn runtime_surfaces_injected_authority_loss_cleanly() {
        let faults = FaultConfig {
            disconnect: Some(DisconnectWindow {
                start_tick: 5,
                reconnect_tick: None,
            }),
            ..FaultConfig::default()
        };
        let (lab, mut client, mut authority) =
            fault_runtime_pair(faults, 0x1057, RuntimeConfig::default());
        for tick in 0..=5 {
            lab.advance_to(tick).unwrap();
            client.pump(SimTick(tick));
            authority.pump(SimTick(tick));
        }
        assert_eq!(
            client.connection_state(),
            RuntimeConnectionState::TransportDisconnected
        );
        assert_eq!(
            authority.connection_state(),
            RuntimeConnectionState::TransportDisconnected
        );
        assert!(matches!(
            client.try_next_event(),
            Some(RuntimeEvent::TransportDisconnected)
        ));
        assert!(matches!(
            authority.try_next_event(),
            Some(RuntimeEvent::TransportDisconnected)
        ));
        assert!(lab.metrics().a_to_b.disconnected_receive_attempts > 0);
        assert!(lab.metrics().b_to_a.disconnected_receive_attempts > 0);
    }
}
