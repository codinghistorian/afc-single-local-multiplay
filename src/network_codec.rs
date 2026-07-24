//! Allocation-free, hostile-input-safe AFC wire codec.
//!
//! The wire representation is explicit and independent of Serde/bincode. All
//! integers are big-endian. Every variable logical collection is decoded into a
//! fixed-capacity stack array only after its count has been checked.

use crate::network_protocol::{
    AuthorityKind, BuildId, ClockProbe, ClockProbeId, ClockReply, CommittedInputRecord,
    CommittedInputRelay, CommittedInputSource, CommittedSeatInputWindow, CompatibilityId,
    DefinitionId, DisconnectCode, DisconnectMessage, FighterId, FighterSlotConfig,
    GameplayContentHash, InputBatch, InputButtons, InputFrame, InputSequence, MAX_FIGHTERS,
    MAX_HIGH_FREQUENCY_PACKET_BYTES, MAX_INPUT_FRAMES_PER_WINDOW, MAX_LOCAL_SEATS,
    MAX_RESYNC_CHUNK_BYTES, MAX_RESYNC_CHUNKS, MAX_RESYNC_INPUT_TAIL_TICKS, MAX_SEATS,
    ManifestHash, MatchId, MatchManifest, PeerId, ProtocolChannel, ProtocolValidationError,
    ProtocolVersion, QuantizedAxis, ReplayFormatVersion, ResyncApplied, ResyncBegin, ResyncChunk,
    ResyncChunkPayload, ResyncInputTail, ResyncReason, ResyncRequest, RetryDisposition,
    SeatAssignment, SeatId, SeatInputWindow, SeatOwner, SeatOwnership, SimTick, SimulationVersion,
    StartMessage, StateBaselineAck, StateHash, TeamId, TransferId,
};
use crate::state_delta::{MAX_STATE_DELTA_BYTES, SnapshotByteDelta};

pub const PACKET_MAGIC: [u8; 4] = *b"AFCN";
pub const PACKET_HEADER_BYTES: usize = 10;
pub const MAX_PACKET_BYTES: usize = MAX_HIGH_FREQUENCY_PACKET_BYTES;
pub type PacketBuffer = [u8; MAX_PACKET_BYTES];

const FIGHTER_SLOT_WIRE_BYTES: usize = 1 + 1 + 1 + 2 + 2 + 2;
const SEAT_ASSIGNMENT_MAX_WIRE_BYTES: usize = 1 + 1 + 1 + 8;
pub const MATCH_MANIFEST_MAX_WIRE_BYTES: usize = Handshake::WIRE_BYTES
    + 8
    + 16
    + 1
    + 1
    + 2
    + 2
    + MAX_FIGHTERS * FIGHTER_SLOT_WIRE_BYTES
    + 1
    + MAX_SEATS * SEAT_ASSIGNMENT_MAX_WIRE_BYTES
    + 8
    + 2
    + 2
    + 1
    + 1
    + 1
    + 8;

const MAGIC_OFFSET: usize = 0;
const PROTOCOL_OFFSET: usize = 4;
const CHANNEL_OFFSET: usize = 6;
const KIND_OFFSET: usize = 7;
const PAYLOAD_LENGTH_OFFSET: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncodeError {
    BufferTooSmall { needed: usize, available: usize },
    PacketTooLarge { size: usize, maximum: usize },
    ProtocolMismatch,
    InvalidMessage(ProtocolValidationError),
}

impl core::fmt::Display for EncodeError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "AFC packet encode failed: {self:?}")
    }
}

impl std::error::Error for EncodeError {}

impl From<ProtocolValidationError> for EncodeError {
    fn from(error: ProtocolValidationError) -> Self {
        Self::InvalidMessage(error)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireField {
    Boolean,
    Channel,
    Kind,
    InputWindowCount,
    InputFrameCount,
    StateAckCount,
    ResyncChunkCount,
    ResyncPayloadLength,
    ResyncReason,
    StateDeltaLength,
    SeatOwnershipCount,
    AuthorityKind,
    SeatOwner,
    CommittedInputSource,
    DisconnectCode,
    RetryDisposition,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    PacketTooShort {
        size: usize,
        minimum: usize,
    },
    PacketTooLarge {
        size: usize,
        maximum: usize,
    },
    BadMagic,
    UnknownProtocol {
        received: u16,
        expected: u16,
    },
    UnknownChannel(u8),
    UnknownKind(u8),
    KindChannelMismatch,
    LengthMismatch {
        declared: usize,
        actual: usize,
    },
    Truncated,
    TrailingBytes(usize),
    InvalidValue {
        field: WireField,
        value: u64,
    },
    LimitExceeded {
        field: WireField,
        value: usize,
        maximum: usize,
    },
    InvalidMessage(ProtocolValidationError),
}

impl core::fmt::Display for DecodeError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "AFC packet decode failed: {self:?}")
    }
}

impl std::error::Error for DecodeError {}

impl From<ProtocolValidationError> for DecodeError {
    fn from(error: ProtocolValidationError) -> Self {
        Self::InvalidMessage(error)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum MessageKind {
    Handshake = 1,
    InputBatch = 2,
    StateHashAndAcks = 3,
    ResyncChunk = 4,
    Disconnect = 5,
    ResultIdentifier = 6,
    Manifest = 7,
    ManifestAccepted = 8,
    InitialSyncApplied = 9,
    Ready = 10,
    Countdown = 11,
    ResyncRequest = 12,
    ResyncBegin = 13,
    ResyncApplied = 14,
    StateDeltaAndAcks = 15,
    CommittedInputRelay = 16,
    ClockProbe = 17,
    ClockReply = 18,
    ResyncInputTail = 19,
}

impl MessageKind {
    pub const fn channel(self) -> ProtocolChannel {
        match self {
            Self::Handshake
            | Self::Disconnect
            | Self::Manifest
            | Self::ManifestAccepted
            | Self::InitialSyncApplied
            | Self::Ready
            | Self::Countdown
            | Self::ResyncRequest
            | Self::ResyncBegin
            | Self::ResyncApplied
            | Self::ClockProbe
            | Self::ClockReply => ProtocolChannel::Control,
            Self::InputBatch | Self::CommittedInputRelay => ProtocolChannel::Input,
            Self::StateHashAndAcks => ProtocolChannel::State,
            Self::StateDeltaAndAcks => ProtocolChannel::State,
            Self::ResyncChunk | Self::ResyncInputTail => ProtocolChannel::Resync,
            Self::ResultIdentifier => ProtocolChannel::Result,
        }
    }

    fn from_wire(value: u8) -> Result<Self, DecodeError> {
        match value {
            1 => Ok(Self::Handshake),
            2 => Ok(Self::InputBatch),
            3 => Ok(Self::StateHashAndAcks),
            4 => Ok(Self::ResyncChunk),
            5 => Ok(Self::Disconnect),
            6 => Ok(Self::ResultIdentifier),
            7 => Ok(Self::Manifest),
            8 => Ok(Self::ManifestAccepted),
            9 => Ok(Self::InitialSyncApplied),
            10 => Ok(Self::Ready),
            11 => Ok(Self::Countdown),
            12 => Ok(Self::ResyncRequest),
            13 => Ok(Self::ResyncBegin),
            14 => Ok(Self::ResyncApplied),
            15 => Ok(Self::StateDeltaAndAcks),
            16 => Ok(Self::CommittedInputRelay),
            17 => Ok(Self::ClockProbe),
            18 => Ok(Self::ClockReply),
            19 => Ok(Self::ResyncInputTail),
            _ => Err(DecodeError::UnknownKind(value)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PacketHeader {
    pub protocol: ProtocolVersion,
    pub channel: ProtocolChannel,
    pub kind: MessageKind,
    pub payload_bytes: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Handshake {
    pub compatibility: CompatibilityId,
}

impl Handshake {
    pub const WIRE_BYTES: usize = 2 + 2 + 2 + 16 + 32;

    pub fn validate(&self) -> Result<(), ProtocolValidationError> {
        self.compatibility.validate()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProcessedInputAck {
    pub seat: SeatId,
    pub processed_through: SimTick,
    pub sequence: InputSequence,
}

impl ProcessedInputAck {
    pub const WIRE_BYTES: usize = 1 + 8 + 2;

    pub fn validate(&self, authority_tick: SimTick) -> Result<(), ProtocolValidationError> {
        self.seat.validate()?;
        if self.processed_through.0 > authority_tick.0 {
            return Err(ProtocolValidationError::FutureInput);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StateHashAndAcks {
    pub match_id: MatchId,
    pub authority_tick: SimTick,
    pub state_hash: StateHash,
    ack_count: u8,
    acks: [ProcessedInputAck; MAX_SEATS],
}

impl StateHashAndAcks {
    pub const MAX_WIRE_BYTES: usize = 16 + 8 + 8 + 1 + MAX_SEATS * ProcessedInputAck::WIRE_BYTES;

    pub fn new(
        match_id: MatchId,
        authority_tick: SimTick,
        state_hash: StateHash,
        acks: &[ProcessedInputAck],
    ) -> Result<Self, ProtocolValidationError> {
        if acks.len() > MAX_SEATS {
            return Err(ProtocolValidationError::CapacityExceeded);
        }
        let mut message = Self {
            match_id,
            authority_tick,
            state_hash,
            ack_count: acks.len() as u8,
            acks: [ProcessedInputAck::default(); MAX_SEATS],
        };
        message.acks[..acks.len()].copy_from_slice(acks);
        message.validate()?;
        Ok(message)
    }

    pub const fn len(&self) -> usize {
        self.ack_count as usize
    }

    pub fn as_slice(&self) -> &[ProcessedInputAck] {
        &self.acks[..self.len().min(MAX_SEATS)]
    }

    pub fn validate(&self) -> Result<(), ProtocolValidationError> {
        self.match_id.validate()?;
        if self.len() > MAX_SEATS {
            return Err(ProtocolValidationError::CapacityExceeded);
        }
        for (index, ack) in self.as_slice().iter().enumerate() {
            ack.validate(self.authority_tick)?;
            if self.as_slice()[..index]
                .iter()
                .any(|prior| prior.seat == ack.seat)
            {
                return Err(ProtocolValidationError::DuplicateSeat);
            }
        }
        if self.acks[self.len()..]
            .iter()
            .any(|ack| *ack != ProcessedInputAck::default())
        {
            return Err(ProtocolValidationError::NonCanonicalPadding);
        }
        Ok(())
    }
}

/// Latest-wins authoritative snapshot patch against a client-acknowledged
/// canonical baseline. A receiver must verify both hashes around patch
/// application before exposing the reconstructed snapshot to rollback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StateDeltaAndAcks {
    pub match_id: MatchId,
    pub base_tick: SimTick,
    pub base_hash: StateHash,
    pub authority_tick: SimTick,
    pub state_hash: StateHash,
    pub delta: SnapshotByteDelta,
    ack_count: u8,
    acks: [ProcessedInputAck; MAX_SEATS],
}

impl StateDeltaAndAcks {
    pub const MAX_WIRE_BYTES: usize = 16
        + 8
        + 8
        + 8
        + 8
        + 4
        + 2
        + 2
        + 1
        + MAX_SEATS * ProcessedInputAck::WIRE_BYTES
        + MAX_STATE_DELTA_BYTES;

    pub fn new(
        match_id: MatchId,
        base_tick: SimTick,
        base_hash: StateHash,
        authority_tick: SimTick,
        state_hash: StateHash,
        delta: SnapshotByteDelta,
        acks: &[ProcessedInputAck],
    ) -> Result<Self, ProtocolValidationError> {
        if acks.len() > MAX_SEATS {
            return Err(ProtocolValidationError::CapacityExceeded);
        }
        let mut message = Self {
            match_id,
            base_tick,
            base_hash,
            authority_tick,
            state_hash,
            delta,
            ack_count: acks.len() as u8,
            acks: [ProcessedInputAck::default(); MAX_SEATS],
        };
        message.acks[..acks.len()].copy_from_slice(acks);
        message.validate()?;
        Ok(message)
    }

    pub const fn len(&self) -> usize {
        self.ack_count as usize
    }

    pub fn as_slice(&self) -> &[ProcessedInputAck] {
        &self.acks[..self.len().min(MAX_SEATS)]
    }

    pub fn validate(&self) -> Result<(), ProtocolValidationError> {
        self.match_id.validate()?;
        if self.base_tick > self.authority_tick || self.delta.target_len() == 0 {
            return Err(ProtocolValidationError::InvalidSnapshot);
        }
        self.delta
            .validate()
            .map_err(|_| ProtocolValidationError::InvalidSnapshot)?;
        if self.len() > MAX_SEATS {
            return Err(ProtocolValidationError::CapacityExceeded);
        }
        for (index, ack) in self.as_slice().iter().enumerate() {
            ack.validate(self.authority_tick)?;
            if self.as_slice()[..index]
                .iter()
                .any(|prior| prior.seat == ack.seat)
            {
                return Err(ProtocolValidationError::DuplicateSeat);
            }
        }
        if self.acks[self.len()..]
            .iter()
            .any(|ack| *ack != ProcessedInputAck::default())
        {
            return Err(ProtocolValidationError::NonCanonicalPadding);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ResultId(u64);

impl ResultId {
    pub fn new(value: u64) -> Result<Self, ProtocolValidationError> {
        if value == 0 {
            Err(ProtocolValidationError::ZeroIdentifier)
        } else {
            Ok(Self(value))
        }
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    pub fn validate(self) -> Result<(), ProtocolValidationError> {
        Self::new(self.0).map(|_| ())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResultIdentifier {
    pub match_id: MatchId,
    pub result_id: ResultId,
    pub final_tick: SimTick,
    pub final_state_hash: StateHash,
}

impl ResultIdentifier {
    pub const WIRE_BYTES: usize = 16 + 8 + 8 + 8;

    pub fn validate(&self) -> Result<(), ProtocolValidationError> {
        self.match_id.validate()?;
        self.result_id.validate()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WireMessage {
    Handshake(Handshake),
    Start(StartMessage),
    InputBatch(InputBatch),
    CommittedInputRelay(CommittedInputRelay),
    StateHashAndAcks(StateHashAndAcks),
    StateDeltaAndAcks(StateDeltaAndAcks),
    ResyncRequest(ResyncRequest),
    ResyncBegin(ResyncBegin),
    ResyncChunk(ResyncChunk),
    ResyncInputTail(ResyncInputTail),
    ResyncApplied(ResyncApplied),
    ClockProbe(ClockProbe),
    ClockReply(ClockReply),
    Disconnect(DisconnectMessage),
    ResultIdentifier(ResultIdentifier),
}

impl WireMessage {
    pub const fn kind(&self) -> MessageKind {
        match self {
            Self::Handshake(_) => MessageKind::Handshake,
            Self::Start(StartMessage::Manifest(_)) => MessageKind::Manifest,
            Self::Start(StartMessage::ManifestAccepted { .. }) => MessageKind::ManifestAccepted,
            Self::Start(StartMessage::InitialSyncApplied { .. }) => MessageKind::InitialSyncApplied,
            Self::Start(StartMessage::Ready { .. }) => MessageKind::Ready,
            Self::Start(StartMessage::Countdown { .. }) => MessageKind::Countdown,
            Self::InputBatch(_) => MessageKind::InputBatch,
            Self::CommittedInputRelay(_) => MessageKind::CommittedInputRelay,
            Self::StateHashAndAcks(_) => MessageKind::StateHashAndAcks,
            Self::StateDeltaAndAcks(_) => MessageKind::StateDeltaAndAcks,
            Self::ResyncRequest(_) => MessageKind::ResyncRequest,
            Self::ResyncBegin(_) => MessageKind::ResyncBegin,
            Self::ResyncChunk(_) => MessageKind::ResyncChunk,
            Self::ResyncInputTail(_) => MessageKind::ResyncInputTail,
            Self::ResyncApplied(_) => MessageKind::ResyncApplied,
            Self::ClockProbe(_) => MessageKind::ClockProbe,
            Self::ClockReply(_) => MessageKind::ClockReply,
            Self::Disconnect(_) => MessageKind::Disconnect,
            Self::ResultIdentifier(_) => MessageKind::ResultIdentifier,
        }
    }

    pub const fn channel(&self) -> ProtocolChannel {
        self.kind().channel()
    }

    fn validate(&self) -> Result<(), ProtocolValidationError> {
        match self {
            Self::Handshake(message) => message.validate(),
            Self::Start(message) => message.validate(),
            Self::InputBatch(message) => message.validate_structure(),
            Self::CommittedInputRelay(message) => message.validate(),
            Self::StateHashAndAcks(message) => message.validate(),
            Self::StateDeltaAndAcks(message) => message.validate(),
            Self::ResyncRequest(message) => message.validate(),
            Self::ResyncBegin(message) => message.validate(),
            Self::ResyncChunk(message) => message.validate(),
            Self::ResyncInputTail(message) => message.validate(),
            Self::ResyncApplied(message) => message.validate(),
            Self::ClockProbe(message) => message.validate(),
            Self::ClockReply(message) => message.validate(),
            Self::Disconnect(message) => message.validate(),
            Self::ResultIdentifier(message) => message.validate(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedPacket {
    pub header: PacketHeader,
    pub message: WireMessage,
}

pub fn encode_packet(
    protocol: ProtocolVersion,
    message: &WireMessage,
    output: &mut [u8],
) -> Result<usize, EncodeError> {
    protocol.validate()?;
    message.validate()?;
    let embedded_compatibility = match message {
        WireMessage::Handshake(handshake) => Some(&handshake.compatibility),
        WireMessage::Start(StartMessage::Manifest(manifest)) => Some(&manifest.compatibility),
        _ => None,
    };
    if embedded_compatibility.is_some_and(|compatibility| compatibility.protocol != protocol) {
        return Err(EncodeError::ProtocolMismatch);
    }
    if output.len() < PACKET_HEADER_BYTES {
        return Err(EncodeError::BufferTooSmall {
            needed: PACKET_HEADER_BYTES,
            available: output.len(),
        });
    }

    let usable = output.len().min(MAX_PACKET_BYTES);
    let payload_bytes = {
        let mut writer = Writer::new(&mut output[PACKET_HEADER_BYTES..usable]);
        encode_body(message, &mut writer)?;
        writer.len()
    };
    let packet_bytes = PACKET_HEADER_BYTES + payload_bytes;
    if packet_bytes > MAX_PACKET_BYTES {
        return Err(EncodeError::PacketTooLarge {
            size: packet_bytes,
            maximum: MAX_PACKET_BYTES,
        });
    }

    output[MAGIC_OFFSET..PROTOCOL_OFFSET].copy_from_slice(&PACKET_MAGIC);
    output[PROTOCOL_OFFSET..CHANNEL_OFFSET].copy_from_slice(&protocol.get().to_be_bytes());
    output[CHANNEL_OFFSET] = channel_to_wire(message.channel());
    output[KIND_OFFSET] = message.kind() as u8;
    output[PAYLOAD_LENGTH_OFFSET..PACKET_HEADER_BYTES]
        .copy_from_slice(&(payload_bytes as u16).to_be_bytes());
    Ok(packet_bytes)
}

pub fn decode_packet(
    packet: &[u8],
    expected: &CompatibilityId,
) -> Result<DecodedPacket, DecodeError> {
    if packet.len() < PACKET_HEADER_BYTES {
        return Err(DecodeError::PacketTooShort {
            size: packet.len(),
            minimum: PACKET_HEADER_BYTES,
        });
    }
    if packet.len() > MAX_PACKET_BYTES {
        return Err(DecodeError::PacketTooLarge {
            size: packet.len(),
            maximum: MAX_PACKET_BYTES,
        });
    }
    expected.validate()?;
    if packet[MAGIC_OFFSET..PROTOCOL_OFFSET] != PACKET_MAGIC {
        return Err(DecodeError::BadMagic);
    }

    let received_protocol =
        u16::from_be_bytes([packet[PROTOCOL_OFFSET], packet[PROTOCOL_OFFSET + 1]]);
    if received_protocol != expected.protocol.get() {
        return Err(DecodeError::UnknownProtocol {
            received: received_protocol,
            expected: expected.protocol.get(),
        });
    }
    let protocol = ProtocolVersion::new(received_protocol)?;
    let channel = channel_from_wire(packet[CHANNEL_OFFSET])?;
    let kind = MessageKind::from_wire(packet[KIND_OFFSET])?;
    if kind.channel() != channel {
        return Err(DecodeError::KindChannelMismatch);
    }
    let declared_payload = usize::from(u16::from_be_bytes([
        packet[PAYLOAD_LENGTH_OFFSET],
        packet[PAYLOAD_LENGTH_OFFSET + 1],
    ]));
    let actual_payload = packet.len() - PACKET_HEADER_BYTES;
    if declared_payload != actual_payload {
        return Err(DecodeError::LengthMismatch {
            declared: declared_payload,
            actual: actual_payload,
        });
    }

    let mut reader = Reader::new(&packet[PACKET_HEADER_BYTES..]);
    let message = decode_body(kind, &mut reader)?;
    if reader.remaining() != 0 {
        return Err(DecodeError::TrailingBytes(reader.remaining()));
    }
    message.validate()?;
    let embedded_compatibility = match &message {
        WireMessage::Handshake(handshake) => Some(&handshake.compatibility),
        WireMessage::Start(StartMessage::Manifest(manifest)) => Some(&manifest.compatibility),
        _ => None,
    };
    if let Some(compatibility) = embedded_compatibility {
        compatibility.validate_against(expected)?;
    }
    Ok(DecodedPacket {
        header: PacketHeader {
            protocol,
            channel,
            kind,
            payload_bytes: declared_payload as u16,
        },
        message,
    })
}

fn encode_body(message: &WireMessage, writer: &mut Writer<'_>) -> Result<(), EncodeError> {
    match message {
        WireMessage::Handshake(message) => encode_compatibility(&message.compatibility, writer),
        WireMessage::Start(message) => encode_start_message(message, writer),
        WireMessage::InputBatch(message) => encode_input_batch(message, writer),
        WireMessage::CommittedInputRelay(message) => encode_committed_input_relay(message, writer),
        WireMessage::StateHashAndAcks(message) => encode_state_hash_and_acks(message, writer),
        WireMessage::StateDeltaAndAcks(message) => encode_state_delta_and_acks(message, writer),
        WireMessage::ResyncRequest(message) => encode_resync_request(message, writer),
        WireMessage::ResyncBegin(message) => encode_resync_begin(message, writer),
        WireMessage::ResyncChunk(message) => encode_resync_chunk(message, writer),
        WireMessage::ResyncInputTail(message) => encode_resync_input_tail(message, writer),
        WireMessage::ResyncApplied(message) => encode_resync_applied(message, writer),
        WireMessage::ClockProbe(message) => encode_clock_probe(message, writer),
        WireMessage::ClockReply(message) => encode_clock_reply(message, writer),
        WireMessage::Disconnect(message) => encode_disconnect(message, writer),
        WireMessage::ResultIdentifier(message) => encode_result_identifier(message, writer),
    }
}

fn decode_body(kind: MessageKind, reader: &mut Reader<'_>) -> Result<WireMessage, DecodeError> {
    match kind {
        MessageKind::Handshake => Ok(WireMessage::Handshake(Handshake {
            compatibility: decode_compatibility(reader)?,
        })),
        MessageKind::Manifest
        | MessageKind::ManifestAccepted
        | MessageKind::InitialSyncApplied
        | MessageKind::Ready
        | MessageKind::Countdown => Ok(WireMessage::Start(decode_start_message(kind, reader)?)),
        MessageKind::InputBatch => Ok(WireMessage::InputBatch(decode_input_batch(reader)?)),
        MessageKind::CommittedInputRelay => Ok(WireMessage::CommittedInputRelay(
            decode_committed_input_relay(reader)?,
        )),
        MessageKind::StateHashAndAcks => Ok(WireMessage::StateHashAndAcks(
            decode_state_hash_and_acks(reader)?,
        )),
        MessageKind::StateDeltaAndAcks => Ok(WireMessage::StateDeltaAndAcks(
            decode_state_delta_and_acks(reader)?,
        )),
        MessageKind::ResyncRequest => {
            Ok(WireMessage::ResyncRequest(decode_resync_request(reader)?))
        }
        MessageKind::ResyncBegin => Ok(WireMessage::ResyncBegin(decode_resync_begin(reader)?)),
        MessageKind::ResyncChunk => Ok(WireMessage::ResyncChunk(decode_resync_chunk(reader)?)),
        MessageKind::ResyncInputTail => Ok(WireMessage::ResyncInputTail(decode_resync_input_tail(
            reader,
        )?)),
        MessageKind::ResyncApplied => {
            Ok(WireMessage::ResyncApplied(decode_resync_applied(reader)?))
        }
        MessageKind::ClockProbe => Ok(WireMessage::ClockProbe(decode_clock_probe(reader)?)),
        MessageKind::ClockReply => Ok(WireMessage::ClockReply(decode_clock_reply(reader)?)),
        MessageKind::Disconnect => Ok(WireMessage::Disconnect(decode_disconnect(reader)?)),
        MessageKind::ResultIdentifier => Ok(WireMessage::ResultIdentifier(
            decode_result_identifier(reader)?,
        )),
    }
}

fn encode_compatibility(
    compatibility: &CompatibilityId,
    writer: &mut Writer<'_>,
) -> Result<(), EncodeError> {
    compatibility.validate()?;
    writer.write_u16(compatibility.protocol.get())?;
    writer.write_u16(compatibility.simulation.get())?;
    writer.write_u16(compatibility.replay.get())?;
    writer.write_bytes(compatibility.build.as_bytes())?;
    writer.write_bytes(compatibility.gameplay_content.as_bytes())
}

fn decode_compatibility(reader: &mut Reader<'_>) -> Result<CompatibilityId, DecodeError> {
    Ok(CompatibilityId {
        protocol: ProtocolVersion::new(reader.read_u16()?)?,
        simulation: SimulationVersion::new(reader.read_u16()?)?,
        replay: ReplayFormatVersion::new(reader.read_u16()?)?,
        build: BuildId::new(reader.read_array::<16>()?)?,
        gameplay_content: GameplayContentHash::new(reader.read_array::<32>()?)?,
    })
}

fn encode_start_message(
    message: &StartMessage,
    writer: &mut Writer<'_>,
) -> Result<(), EncodeError> {
    message.validate()?;
    match message {
        StartMessage::Manifest(manifest) => encode_match_manifest(manifest, writer),
        StartMessage::ManifestAccepted {
            match_id,
            peer_id,
            manifest_hash,
        } => {
            writer.write_bytes(match_id.as_bytes())?;
            writer.write_u64(peer_id.get())?;
            writer.write_u64(manifest_hash.0)
        }
        StartMessage::InitialSyncApplied {
            match_id,
            peer_id,
            snapshot_tick,
            snapshot_hash,
        } => {
            writer.write_bytes(match_id.as_bytes())?;
            writer.write_u64(peer_id.get())?;
            writer.write_u64(snapshot_tick.0)?;
            writer.write_u64(snapshot_hash.0)
        }
        StartMessage::Ready { match_id, peer_id } => {
            writer.write_bytes(match_id.as_bytes())?;
            writer.write_u64(peer_id.get())
        }
        StartMessage::Countdown {
            match_id,
            start_tick,
        } => {
            writer.write_bytes(match_id.as_bytes())?;
            writer.write_u64(start_tick.0)
        }
    }
}

fn decode_start_message(
    kind: MessageKind,
    reader: &mut Reader<'_>,
) -> Result<StartMessage, DecodeError> {
    let message = match kind {
        MessageKind::Manifest => StartMessage::Manifest(decode_match_manifest(reader)?),
        MessageKind::ManifestAccepted => StartMessage::ManifestAccepted {
            match_id: MatchId::new(reader.read_array::<16>()?)?,
            peer_id: PeerId::new(reader.read_u64()?)?,
            manifest_hash: ManifestHash(reader.read_u64()?),
        },
        MessageKind::InitialSyncApplied => StartMessage::InitialSyncApplied {
            match_id: MatchId::new(reader.read_array::<16>()?)?,
            peer_id: PeerId::new(reader.read_u64()?)?,
            snapshot_tick: SimTick(reader.read_u64()?),
            snapshot_hash: StateHash(reader.read_u64()?),
        },
        MessageKind::Ready => StartMessage::Ready {
            match_id: MatchId::new(reader.read_array::<16>()?)?,
            peer_id: PeerId::new(reader.read_u64()?)?,
        },
        MessageKind::Countdown => StartMessage::Countdown {
            match_id: MatchId::new(reader.read_array::<16>()?)?,
            start_tick: SimTick(reader.read_u64()?),
        },
        _ => return Err(DecodeError::KindChannelMismatch),
    };
    message.validate()?;
    Ok(message)
}

fn encode_match_manifest(
    manifest: &MatchManifest,
    writer: &mut Writer<'_>,
) -> Result<(), EncodeError> {
    manifest.validate()?;
    encode_compatibility(&manifest.compatibility, writer)?;
    writer.write_u64(manifest.manifest_hash.0)?;
    writer.write_bytes(manifest.match_id.as_bytes())?;
    writer.write_u8(authority_kind_to_wire(manifest.authority))?;
    writer.write_u8(u8::from(manifest.trusted_results))?;
    writer.write_u16(manifest.arena.get())?;
    writer.write_u16(manifest.rules.get())?;
    for slot in &manifest.slots {
        encode_fighter_slot(slot, writer)?;
    }
    writer.write_u8(manifest.ownership.len() as u8)?;
    for assignment in manifest.ownership.as_slice() {
        encode_seat_assignment(assignment, writer)?;
    }
    writer.write_u64(manifest.master_gameplay_seed)?;
    writer.write_u16(manifest.rng_scheme_version)?;
    writer.write_u16(manifest.tick_rate_hz)?;
    writer.write_u8(manifest.input_delay_ticks)?;
    writer.write_u8(manifest.rollback_limit_ticks)?;
    writer.write_u8(manifest.snapshot_history_ticks)?;
    writer.write_u64(manifest.agreed_start_tick.0)
}

fn decode_match_manifest(reader: &mut Reader<'_>) -> Result<MatchManifest, DecodeError> {
    let compatibility = decode_compatibility(reader)?;
    let manifest_hash = ManifestHash(reader.read_u64()?);
    let match_id = MatchId::new(reader.read_array::<16>()?)?;
    let authority = authority_kind_from_wire(reader.read_u8()?)?;
    let trusted_results = read_bool(reader)?;
    let arena = DefinitionId::new(reader.read_u16()?)?;
    let rules = DefinitionId::new(reader.read_u16()?)?;

    let mut slots = [FighterSlotConfig::default(); MAX_FIGHTERS];
    for slot in &mut slots {
        *slot = decode_fighter_slot(reader)?;
    }

    let ownership_count = usize::from(reader.read_u8()?);
    if ownership_count > MAX_SEATS {
        return Err(DecodeError::LimitExceeded {
            field: WireField::SeatOwnershipCount,
            value: ownership_count,
            maximum: MAX_SEATS,
        });
    }
    let mut assignments = [SeatAssignment::default(); MAX_SEATS];
    for assignment in &mut assignments[..ownership_count] {
        *assignment = decode_seat_assignment(reader)?;
    }
    let ownership = SeatOwnership::from_assignments(&assignments[..ownership_count])?;

    let manifest = MatchManifest {
        compatibility,
        manifest_hash,
        match_id,
        authority,
        trusted_results,
        arena,
        rules,
        slots,
        ownership,
        master_gameplay_seed: reader.read_u64()?,
        rng_scheme_version: reader.read_u16()?,
        tick_rate_hz: reader.read_u16()?,
        input_delay_ticks: reader.read_u8()?,
        rollback_limit_ticks: reader.read_u8()?,
        snapshot_history_ticks: reader.read_u8()?,
        agreed_start_tick: SimTick(reader.read_u64()?),
    };
    manifest.validate()?;
    Ok(manifest)
}

fn encode_fighter_slot(
    slot: &FighterSlotConfig,
    writer: &mut Writer<'_>,
) -> Result<(), EncodeError> {
    writer.write_u8(u8::from(slot.occupied))?;
    writer.write_u8(slot.fighter.get())?;
    writer.write_u8(slot.team.get())?;
    writer.write_u16(slot.character.get())?;
    writer.write_u16(slot.style.get())?;
    writer.write_u16(slot.equipment.get())
}

fn decode_fighter_slot(reader: &mut Reader<'_>) -> Result<FighterSlotConfig, DecodeError> {
    Ok(FighterSlotConfig {
        occupied: read_bool(reader)?,
        fighter: FighterId::new(reader.read_u8()?)
            .ok_or(ProtocolValidationError::InvalidFighter)?,
        team: TeamId::new(reader.read_u8()?)?,
        character: DefinitionId::new(reader.read_u16()?)?,
        style: DefinitionId::new(reader.read_u16()?)?,
        equipment: DefinitionId::new(reader.read_u16()?)?,
    })
}

fn encode_seat_assignment(
    assignment: &SeatAssignment,
    writer: &mut Writer<'_>,
) -> Result<(), EncodeError> {
    assignment.validate()?;
    writer.write_u8(assignment.seat.get())?;
    writer.write_u8(assignment.fighter.get())?;
    match assignment.owner {
        SeatOwner::AuthorityBot => writer.write_u8(1),
        SeatOwner::Peer(peer_id) => {
            writer.write_u8(2)?;
            writer.write_u64(peer_id.get())
        }
    }
}

fn decode_seat_assignment(reader: &mut Reader<'_>) -> Result<SeatAssignment, DecodeError> {
    let seat = SeatId::new(reader.read_u8()?)?;
    let fighter =
        FighterId::new(reader.read_u8()?).ok_or(ProtocolValidationError::InvalidFighter)?;
    let owner = match reader.read_u8()? {
        1 => SeatOwner::AuthorityBot,
        2 => SeatOwner::Peer(PeerId::new(reader.read_u64()?)?),
        value => {
            return Err(DecodeError::InvalidValue {
                field: WireField::SeatOwner,
                value: u64::from(value),
            });
        }
    };
    let assignment = SeatAssignment {
        seat,
        fighter,
        owner,
    };
    assignment.validate()?;
    Ok(assignment)
}

fn encode_input_batch(message: &InputBatch, writer: &mut Writer<'_>) -> Result<(), EncodeError> {
    message.validate_structure()?;
    writer.write_bytes(message.match_id.as_bytes())?;
    writer.write_u64(message.peer_id.get())?;
    writer.write_u8(message.len() as u8)?;
    for window in message.as_slice() {
        writer.write_u8(window.len() as u8)?;
        for frame in window.as_slice() {
            encode_input_frame(frame, writer)?;
        }
    }
    match message.state_baseline_ack() {
        Some(acknowledgement) => {
            writer.write_u8(1)?;
            writer.write_u64(acknowledgement.tick.0)?;
            writer.write_u64(acknowledgement.hash.0)
        }
        None => writer.write_u8(0),
    }
}

fn decode_input_batch(reader: &mut Reader<'_>) -> Result<InputBatch, DecodeError> {
    let match_id = MatchId::new(reader.read_array::<16>()?)?;
    let peer_id = PeerId::new(reader.read_u64()?)?;
    let window_count = usize::from(reader.read_u8()?);
    if window_count == 0 {
        return Err(DecodeError::InvalidValue {
            field: WireField::InputWindowCount,
            value: 0,
        });
    }
    if window_count > MAX_LOCAL_SEATS as usize {
        return Err(DecodeError::LimitExceeded {
            field: WireField::InputWindowCount,
            value: window_count,
            maximum: MAX_LOCAL_SEATS as usize,
        });
    }

    let mut windows = [SeatInputWindow::default(); MAX_LOCAL_SEATS as usize];
    for window in &mut windows[..window_count] {
        let frame_count = usize::from(reader.read_u8()?);
        if frame_count == 0 {
            return Err(DecodeError::InvalidValue {
                field: WireField::InputFrameCount,
                value: 0,
            });
        }
        if frame_count > MAX_INPUT_FRAMES_PER_WINDOW {
            return Err(DecodeError::LimitExceeded {
                field: WireField::InputFrameCount,
                value: frame_count,
                maximum: MAX_INPUT_FRAMES_PER_WINDOW,
            });
        }
        let mut frames = [InputFrame::default(); MAX_INPUT_FRAMES_PER_WINDOW];
        for frame in &mut frames[..frame_count] {
            *frame = decode_input_frame(reader)?;
        }
        *window = SeatInputWindow::from_newest_first(&frames[..frame_count])?;
    }
    let batch = InputBatch::new(match_id, peer_id, &windows[..window_count])?;
    if read_bool(reader)? {
        Ok(batch.with_state_baseline_ack(StateBaselineAck {
            tick: SimTick(reader.read_u64()?),
            hash: StateHash(reader.read_u64()?),
        })?)
    } else {
        Ok(batch)
    }
}

fn encode_committed_input_relay(
    message: &CommittedInputRelay,
    writer: &mut Writer<'_>,
) -> Result<(), EncodeError> {
    message.validate()?;
    writer.write_bytes(message.match_id.as_bytes())?;
    writer.write_u64(message.authority_tick.0)?;
    writer.write_u8(message.len() as u8)?;
    for window in message.as_slice() {
        writer.write_u8(window.len() as u8)?;
        for record in window.as_slice() {
            encode_committed_input_record(record, writer)?;
        }
    }
    Ok(())
}

fn decode_committed_input_relay(
    reader: &mut Reader<'_>,
) -> Result<CommittedInputRelay, DecodeError> {
    let match_id = MatchId::new(reader.read_array::<16>()?)?;
    let authority_tick = SimTick(reader.read_u64()?);
    let window_count = usize::from(reader.read_u8()?);
    if window_count == 0 {
        return Err(DecodeError::InvalidValue {
            field: WireField::InputWindowCount,
            value: 0,
        });
    }
    if window_count > MAX_SEATS {
        return Err(DecodeError::LimitExceeded {
            field: WireField::InputWindowCount,
            value: window_count,
            maximum: MAX_SEATS,
        });
    }

    let mut windows = [CommittedSeatInputWindow::default(); MAX_SEATS];
    for window in &mut windows[..window_count] {
        let record_count = usize::from(reader.read_u8()?);
        if record_count == 0 {
            return Err(DecodeError::InvalidValue {
                field: WireField::InputFrameCount,
                value: 0,
            });
        }
        if record_count > MAX_INPUT_FRAMES_PER_WINDOW {
            return Err(DecodeError::LimitExceeded {
                field: WireField::InputFrameCount,
                value: record_count,
                maximum: MAX_INPUT_FRAMES_PER_WINDOW,
            });
        }
        let mut records = [CommittedInputRecord::default(); MAX_INPUT_FRAMES_PER_WINDOW];
        for record in &mut records[..record_count] {
            *record = decode_committed_input_record(reader)?;
        }
        *window = CommittedSeatInputWindow::from_newest_first(&records[..record_count])?;
    }
    Ok(CommittedInputRelay::new(
        match_id,
        authority_tick,
        &windows[..window_count],
    )?)
}

fn encode_input_frame(frame: &InputFrame, writer: &mut Writer<'_>) -> Result<(), EncodeError> {
    frame.validate()?;
    writer.write_u64(frame.tick.0)?;
    writer.write_u8(frame.seat.get())?;
    writer.write_i8(frame.movement_x.get())?;
    writer.write_i8(frame.movement_y.get())?;
    writer.write_u16(frame.held_buttons.bits())?;
    writer.write_u16(frame.pressed_buttons.bits())?;
    writer.write_u16(frame.released_buttons.bits())?;
    writer.write_u16(frame.sequence.0)
}

fn decode_input_frame(reader: &mut Reader<'_>) -> Result<InputFrame, DecodeError> {
    let frame = InputFrame {
        tick: SimTick(reader.read_u64()?),
        seat: SeatId::new(reader.read_u8()?)?,
        movement_x: QuantizedAxis::new(reader.read_i8()?)?,
        movement_y: QuantizedAxis::new(reader.read_i8()?)?,
        held_buttons: InputButtons::new(reader.read_u16()?)?,
        pressed_buttons: InputButtons::new(reader.read_u16()?)?,
        released_buttons: InputButtons::new(reader.read_u16()?)?,
        sequence: InputSequence(reader.read_u16()?),
    };
    frame.validate()?;
    Ok(frame)
}

fn encode_state_hash_and_acks(
    message: &StateHashAndAcks,
    writer: &mut Writer<'_>,
) -> Result<(), EncodeError> {
    message.validate()?;
    writer.write_bytes(message.match_id.as_bytes())?;
    writer.write_u64(message.authority_tick.0)?;
    writer.write_u64(message.state_hash.0)?;
    writer.write_u8(message.len() as u8)?;
    for ack in message.as_slice() {
        writer.write_u8(ack.seat.get())?;
        writer.write_u64(ack.processed_through.0)?;
        writer.write_u16(ack.sequence.0)?;
    }
    Ok(())
}

fn decode_state_hash_and_acks(reader: &mut Reader<'_>) -> Result<StateHashAndAcks, DecodeError> {
    let match_id = MatchId::new(reader.read_array::<16>()?)?;
    let authority_tick = SimTick(reader.read_u64()?);
    let state_hash = StateHash(reader.read_u64()?);
    let ack_count = usize::from(reader.read_u8()?);
    if ack_count > MAX_SEATS {
        return Err(DecodeError::LimitExceeded {
            field: WireField::StateAckCount,
            value: ack_count,
            maximum: MAX_SEATS,
        });
    }
    let mut acks = [ProcessedInputAck::default(); MAX_SEATS];
    for ack in &mut acks[..ack_count] {
        *ack = ProcessedInputAck {
            seat: SeatId::new(reader.read_u8()?)?,
            processed_through: SimTick(reader.read_u64()?),
            sequence: InputSequence(reader.read_u16()?),
        };
    }
    Ok(StateHashAndAcks::new(
        match_id,
        authority_tick,
        state_hash,
        &acks[..ack_count],
    )?)
}

fn encode_state_delta_and_acks(
    message: &StateDeltaAndAcks,
    writer: &mut Writer<'_>,
) -> Result<(), EncodeError> {
    message.validate()?;
    writer.write_bytes(message.match_id.as_bytes())?;
    writer.write_u64(message.base_tick.0)?;
    writer.write_u64(message.base_hash.0)?;
    writer.write_u64(message.authority_tick.0)?;
    writer.write_u64(message.state_hash.0)?;
    writer.write_u32(message.delta.target_len() as u32)?;
    writer.write_u16(message.delta.run_count())?;
    writer.write_u16(message.delta.payload_len() as u16)?;
    writer.write_u8(message.len() as u8)?;
    for ack in message.as_slice() {
        writer.write_u8(ack.seat.get())?;
        writer.write_u64(ack.processed_through.0)?;
        writer.write_u16(ack.sequence.0)?;
    }
    writer.write_bytes(message.delta.payload())
}

fn decode_state_delta_and_acks(reader: &mut Reader<'_>) -> Result<StateDeltaAndAcks, DecodeError> {
    let match_id = MatchId::new(reader.read_array::<16>()?)?;
    let base_tick = SimTick(reader.read_u64()?);
    let base_hash = StateHash(reader.read_u64()?);
    let authority_tick = SimTick(reader.read_u64()?);
    let state_hash = StateHash(reader.read_u64()?);
    let target_len = reader.read_u32()?;
    let run_count = reader.read_u16()?;
    let payload_len = usize::from(reader.read_u16()?);
    if payload_len > MAX_STATE_DELTA_BYTES {
        return Err(DecodeError::LimitExceeded {
            field: WireField::StateDeltaLength,
            value: payload_len,
            maximum: MAX_STATE_DELTA_BYTES,
        });
    }
    let ack_count = usize::from(reader.read_u8()?);
    if ack_count > MAX_SEATS {
        return Err(DecodeError::LimitExceeded {
            field: WireField::StateAckCount,
            value: ack_count,
            maximum: MAX_SEATS,
        });
    }
    let mut acks = [ProcessedInputAck::default(); MAX_SEATS];
    for ack in &mut acks[..ack_count] {
        *ack = ProcessedInputAck {
            seat: SeatId::new(reader.read_u8()?)?,
            processed_through: SimTick(reader.read_u64()?),
            sequence: InputSequence(reader.read_u16()?),
        };
    }
    let payload = reader.read_bytes(payload_len)?;
    let delta = SnapshotByteDelta::from_wire_parts(target_len, run_count, payload)
        .map_err(|_| ProtocolValidationError::InvalidSnapshot)?;
    Ok(StateDeltaAndAcks::new(
        match_id,
        base_tick,
        base_hash,
        authority_tick,
        state_hash,
        delta,
        &acks[..ack_count],
    )?)
}

fn encode_resync_request(
    message: &ResyncRequest,
    writer: &mut Writer<'_>,
) -> Result<(), EncodeError> {
    message.validate()?;
    writer.write_bytes(message.match_id.as_bytes())?;
    writer.write_u64(message.peer_id.get())?;
    writer.write_u8(resync_reason_to_wire(message.reason))?;
    writer.write_u64(message.last_confirmed_tick.0)?;
    writer.write_u64(message.last_confirmed_hash.0)
}

fn decode_resync_request(reader: &mut Reader<'_>) -> Result<ResyncRequest, DecodeError> {
    let message = ResyncRequest {
        match_id: MatchId::new(reader.read_array::<16>()?)?,
        peer_id: PeerId::new(reader.read_u64()?)?,
        reason: resync_reason_from_wire(reader.read_u8()?)?,
        last_confirmed_tick: SimTick(reader.read_u64()?),
        last_confirmed_hash: StateHash(reader.read_u64()?),
    };
    message.validate()?;
    Ok(message)
}

fn encode_resync_begin(message: &ResyncBegin, writer: &mut Writer<'_>) -> Result<(), EncodeError> {
    message.validate()?;
    writer.write_bytes(message.match_id.as_bytes())?;
    writer.write_u32(message.transfer_id.get())?;
    writer.write_u64(message.snapshot_tick.0)?;
    writer.write_u64(message.snapshot_hash.0)?;
    writer.write_u32(message.snapshot_bytes)?;
    writer.write_u16(message.chunk_count)?;
    writer.write_u64(message.recent_input_start.0)?;
    writer.write_u64(message.recent_input_end.0)
}

fn decode_resync_begin(reader: &mut Reader<'_>) -> Result<ResyncBegin, DecodeError> {
    let message = ResyncBegin {
        match_id: MatchId::new(reader.read_array::<16>()?)?,
        transfer_id: TransferId::new(reader.read_u32()?)?,
        snapshot_tick: SimTick(reader.read_u64()?),
        snapshot_hash: StateHash(reader.read_u64()?),
        snapshot_bytes: reader.read_u32()?,
        chunk_count: reader.read_u16()?,
        recent_input_start: SimTick(reader.read_u64()?),
        recent_input_end: SimTick(reader.read_u64()?),
    };
    message.validate()?;
    Ok(message)
}

fn encode_resync_chunk(message: &ResyncChunk, writer: &mut Writer<'_>) -> Result<(), EncodeError> {
    message.validate()?;
    writer.write_bytes(message.match_id.as_bytes())?;
    writer.write_u32(message.transfer_id.get())?;
    writer.write_u64(message.snapshot_tick.0)?;
    writer.write_u64(message.snapshot_hash.0)?;
    writer.write_u16(message.chunk_index)?;
    writer.write_u16(message.chunk_count)?;
    writer.write_u16(message.payload_len)?;

    let mut payload = [0_u8; MAX_RESYNC_CHUNK_BYTES];
    message
        .payload
        .copy_prefix_into(message.payload_len, &mut payload)?;
    writer.write_bytes(&payload)
}

fn decode_resync_chunk(reader: &mut Reader<'_>) -> Result<ResyncChunk, DecodeError> {
    let match_id = MatchId::new(reader.read_array::<16>()?)?;
    let transfer_id = TransferId::new(reader.read_u32()?)?;
    let snapshot_tick = SimTick(reader.read_u64()?);
    let snapshot_hash = StateHash(reader.read_u64()?);
    let chunk_index = reader.read_u16()?;
    let chunk_count = reader.read_u16()?;
    if chunk_count == 0 {
        return Err(DecodeError::InvalidValue {
            field: WireField::ResyncChunkCount,
            value: 0,
        });
    }
    if usize::from(chunk_count) > MAX_RESYNC_CHUNKS {
        return Err(DecodeError::LimitExceeded {
            field: WireField::ResyncChunkCount,
            value: usize::from(chunk_count),
            maximum: MAX_RESYNC_CHUNKS,
        });
    }
    let payload_len = reader.read_u16()?;
    if payload_len == 0 {
        return Err(DecodeError::InvalidValue {
            field: WireField::ResyncPayloadLength,
            value: 0,
        });
    }
    if usize::from(payload_len) > MAX_RESYNC_CHUNK_BYTES {
        return Err(DecodeError::LimitExceeded {
            field: WireField::ResyncPayloadLength,
            value: usize::from(payload_len),
            maximum: MAX_RESYNC_CHUNK_BYTES,
        });
    }
    let raw_payload = reader.read_array::<MAX_RESYNC_CHUNK_BYTES>()?;
    if raw_payload[usize::from(payload_len)..]
        .iter()
        .any(|byte| *byte != 0)
    {
        return Err(ProtocolValidationError::NonZeroChunkPadding.into());
    }
    let (payload, canonical_len) =
        ResyncChunkPayload::from_bytes(&raw_payload[..usize::from(payload_len)])?;
    debug_assert_eq!(canonical_len, payload_len);
    let message = ResyncChunk {
        match_id,
        transfer_id,
        snapshot_tick,
        snapshot_hash,
        chunk_index,
        chunk_count,
        payload_len,
        payload,
    };
    message.validate()?;
    Ok(message)
}

fn encode_resync_input_tail(
    message: &ResyncInputTail,
    writer: &mut Writer<'_>,
) -> Result<(), EncodeError> {
    message.validate()?;
    writer.write_bytes(message.match_id.as_bytes())?;
    writer.write_u32(message.transfer_id.get())?;
    writer.write_u64(message.snapshot_tick.0)?;
    writer.write_u64(message.snapshot_hash.0)?;
    writer.write_u64(message.recent_input_start.0)?;
    writer.write_u64(message.recent_input_end.0)?;
    writer.write_u8(message.len() as u8)?;
    for window in message.as_slice() {
        writer.write_u8(window.len() as u8)?;
        for record in window.as_slice() {
            encode_committed_input_record(record, writer)?;
        }
    }
    Ok(())
}

fn decode_resync_input_tail(reader: &mut Reader<'_>) -> Result<ResyncInputTail, DecodeError> {
    let match_id = MatchId::new(reader.read_array::<16>()?)?;
    let transfer_id = TransferId::new(reader.read_u32()?)?;
    let snapshot_tick = SimTick(reader.read_u64()?);
    let snapshot_hash = StateHash(reader.read_u64()?);
    let recent_input_start = SimTick(reader.read_u64()?);
    let recent_input_end = SimTick(reader.read_u64()?);
    let window_count = usize::from(reader.read_u8()?);
    if window_count == 0 {
        return Err(DecodeError::InvalidValue {
            field: WireField::InputWindowCount,
            value: 0,
        });
    }
    if window_count > MAX_SEATS {
        return Err(DecodeError::LimitExceeded {
            field: WireField::InputWindowCount,
            value: window_count,
            maximum: MAX_SEATS,
        });
    }
    let mut windows = [CommittedSeatInputWindow::default(); MAX_SEATS];
    for window in &mut windows[..window_count] {
        let record_count = usize::from(reader.read_u8()?);
        if record_count == 0 {
            return Err(DecodeError::InvalidValue {
                field: WireField::InputFrameCount,
                value: 0,
            });
        }
        if record_count > MAX_RESYNC_INPUT_TAIL_TICKS {
            return Err(DecodeError::LimitExceeded {
                field: WireField::InputFrameCount,
                value: record_count,
                maximum: MAX_RESYNC_INPUT_TAIL_TICKS,
            });
        }
        let mut records = [CommittedInputRecord::default(); MAX_RESYNC_INPUT_TAIL_TICKS];
        for record in &mut records[..record_count] {
            *record = decode_committed_input_record(reader)?;
        }
        *window = CommittedSeatInputWindow::from_newest_first(&records[..record_count])?;
    }
    Ok(ResyncInputTail::from_parts(
        match_id,
        transfer_id,
        snapshot_tick,
        snapshot_hash,
        recent_input_start,
        recent_input_end,
        &windows[..window_count],
    )?)
}

fn encode_committed_input_record(
    record: &CommittedInputRecord,
    writer: &mut Writer<'_>,
) -> Result<(), EncodeError> {
    record.validate()?;
    encode_input_frame(&record.frame, writer)?;
    writer.write_u8(record.fighter.get())?;
    match record.source {
        CommittedInputSource::Peer(peer) => {
            writer.write_u8(1)?;
            writer.write_u64(peer.get())
        }
        CommittedInputSource::AuthorityBot => {
            writer.write_u8(2)?;
            writer.write_u64(0)
        }
        CommittedInputSource::MissingSubstitute => {
            writer.write_u8(3)?;
            writer.write_u64(0)
        }
    }
}

fn decode_committed_input_record(
    reader: &mut Reader<'_>,
) -> Result<CommittedInputRecord, DecodeError> {
    let frame = decode_input_frame(reader)?;
    let fighter =
        FighterId::new(reader.read_u8()?).ok_or(ProtocolValidationError::InvalidFighter)?;
    let source_tag = reader.read_u8()?;
    let source_peer = reader.read_u64()?;
    let source = match source_tag {
        1 => CommittedInputSource::Peer(PeerId::new(source_peer)?),
        2 if source_peer == 0 => CommittedInputSource::AuthorityBot,
        3 if source_peer == 0 => CommittedInputSource::MissingSubstitute,
        2 | 3 => return Err(ProtocolValidationError::NonCanonicalPadding.into()),
        value => {
            return Err(DecodeError::InvalidValue {
                field: WireField::CommittedInputSource,
                value: u64::from(value),
            });
        }
    };
    let record = CommittedInputRecord {
        frame,
        fighter,
        source,
    };
    record.validate()?;
    Ok(record)
}

fn encode_resync_applied(
    message: &ResyncApplied,
    writer: &mut Writer<'_>,
) -> Result<(), EncodeError> {
    message.validate()?;
    writer.write_bytes(message.match_id.as_bytes())?;
    writer.write_u32(message.transfer_id.get())?;
    writer.write_u64(message.peer_id.get())?;
    writer.write_u64(message.snapshot_tick.0)?;
    writer.write_u64(message.snapshot_hash.0)
}

fn decode_resync_applied(reader: &mut Reader<'_>) -> Result<ResyncApplied, DecodeError> {
    let message = ResyncApplied {
        match_id: MatchId::new(reader.read_array::<16>()?)?,
        transfer_id: TransferId::new(reader.read_u32()?)?,
        peer_id: PeerId::new(reader.read_u64()?)?,
        snapshot_tick: SimTick(reader.read_u64()?),
        snapshot_hash: StateHash(reader.read_u64()?),
    };
    message.validate()?;
    Ok(message)
}

fn encode_clock_probe(message: &ClockProbe, writer: &mut Writer<'_>) -> Result<(), EncodeError> {
    message.validate()?;
    writer.write_bytes(message.match_id.as_bytes())?;
    writer.write_u64(message.peer_id.get())?;
    writer.write_u32(message.probe_id.get())
}

fn decode_clock_probe(reader: &mut Reader<'_>) -> Result<ClockProbe, DecodeError> {
    let message = ClockProbe {
        match_id: MatchId::new(reader.read_array::<16>()?)?,
        peer_id: PeerId::new(reader.read_u64()?)?,
        probe_id: ClockProbeId::new(reader.read_u32()?)?,
    };
    message.validate()?;
    Ok(message)
}

fn encode_clock_reply(message: &ClockReply, writer: &mut Writer<'_>) -> Result<(), EncodeError> {
    message.validate()?;
    writer.write_bytes(message.match_id.as_bytes())?;
    writer.write_u64(message.peer_id.get())?;
    writer.write_u32(message.probe_id.get())?;
    writer.write_u64(message.authority_tick.0)
}

fn decode_clock_reply(reader: &mut Reader<'_>) -> Result<ClockReply, DecodeError> {
    let message = ClockReply {
        match_id: MatchId::new(reader.read_array::<16>()?)?,
        peer_id: PeerId::new(reader.read_u64()?)?,
        probe_id: ClockProbeId::new(reader.read_u32()?)?,
        authority_tick: SimTick(reader.read_u64()?),
    };
    message.validate()?;
    Ok(message)
}

fn encode_disconnect(
    message: &DisconnectMessage,
    writer: &mut Writer<'_>,
) -> Result<(), EncodeError> {
    message.validate()?;
    match message.match_id {
        Some(match_id) => {
            writer.write_u8(1)?;
            writer.write_bytes(match_id.as_bytes())?;
        }
        None => writer.write_u8(0)?,
    }
    writer.write_u8(disconnect_code_to_wire(message.code))?;
    writer.write_u8(retry_to_wire(message.retry))?;
    writer.write_u16(message.detail_code)?;
    match message.last_confirmed_tick {
        Some(tick) => {
            writer.write_u8(1)?;
            writer.write_u64(tick.0)
        }
        None => writer.write_u8(0),
    }
}

fn decode_disconnect(reader: &mut Reader<'_>) -> Result<DisconnectMessage, DecodeError> {
    let match_id = if read_bool(reader)? {
        Some(MatchId::new(reader.read_array::<16>()?)?)
    } else {
        None
    };
    let code = disconnect_code_from_wire(reader.read_u8()?)?;
    let retry = retry_from_wire(reader.read_u8()?)?;
    let detail_code = reader.read_u16()?;
    let last_confirmed_tick = if read_bool(reader)? {
        Some(SimTick(reader.read_u64()?))
    } else {
        None
    };
    let message = DisconnectMessage {
        match_id,
        code,
        retry,
        detail_code,
        last_confirmed_tick,
    };
    message.validate()?;
    Ok(message)
}

fn encode_result_identifier(
    message: &ResultIdentifier,
    writer: &mut Writer<'_>,
) -> Result<(), EncodeError> {
    message.validate()?;
    writer.write_bytes(message.match_id.as_bytes())?;
    writer.write_u64(message.result_id.get())?;
    writer.write_u64(message.final_tick.0)?;
    writer.write_u64(message.final_state_hash.0)
}

fn decode_result_identifier(reader: &mut Reader<'_>) -> Result<ResultIdentifier, DecodeError> {
    let message = ResultIdentifier {
        match_id: MatchId::new(reader.read_array::<16>()?)?,
        result_id: ResultId::new(reader.read_u64()?)?,
        final_tick: SimTick(reader.read_u64()?),
        final_state_hash: StateHash(reader.read_u64()?),
    };
    message.validate()?;
    Ok(message)
}

fn read_bool(reader: &mut Reader<'_>) -> Result<bool, DecodeError> {
    match reader.read_u8()? {
        0 => Ok(false),
        1 => Ok(true),
        value => Err(DecodeError::InvalidValue {
            field: WireField::Boolean,
            value: u64::from(value),
        }),
    }
}

const fn resync_reason_to_wire(reason: ResyncReason) -> u8 {
    match reason {
        ResyncReason::InitialSync => 0,
        ResyncReason::Reconnect => 1,
        ResyncReason::HashMismatch => 2,
        ResyncReason::HistoryExpired => 3,
    }
}

fn resync_reason_from_wire(value: u8) -> Result<ResyncReason, DecodeError> {
    match value {
        0 => Ok(ResyncReason::InitialSync),
        1 => Ok(ResyncReason::Reconnect),
        2 => Ok(ResyncReason::HashMismatch),
        3 => Ok(ResyncReason::HistoryExpired),
        value => Err(DecodeError::InvalidValue {
            field: WireField::ResyncReason,
            value: u64::from(value),
        }),
    }
}

const fn channel_to_wire(channel: ProtocolChannel) -> u8 {
    match channel {
        ProtocolChannel::Control => 1,
        ProtocolChannel::Input => 2,
        ProtocolChannel::State => 3,
        ProtocolChannel::Resync => 4,
        ProtocolChannel::Result => 5,
    }
}

fn channel_from_wire(value: u8) -> Result<ProtocolChannel, DecodeError> {
    match value {
        1 => Ok(ProtocolChannel::Control),
        2 => Ok(ProtocolChannel::Input),
        3 => Ok(ProtocolChannel::State),
        4 => Ok(ProtocolChannel::Resync),
        5 => Ok(ProtocolChannel::Result),
        _ => Err(DecodeError::UnknownChannel(value)),
    }
}

const fn authority_kind_to_wire(authority: AuthorityKind) -> u8 {
    match authority {
        AuthorityKind::Offline => 1,
        AuthorityKind::Listen => 2,
        AuthorityKind::Dedicated => 3,
    }
}

fn authority_kind_from_wire(value: u8) -> Result<AuthorityKind, DecodeError> {
    match value {
        1 => Ok(AuthorityKind::Offline),
        2 => Ok(AuthorityKind::Listen),
        3 => Ok(AuthorityKind::Dedicated),
        _ => Err(DecodeError::InvalidValue {
            field: WireField::AuthorityKind,
            value: u64::from(value),
        }),
    }
}

const fn disconnect_code_to_wire(code: DisconnectCode) -> u8 {
    match code {
        DisconnectCode::ClientRequested => 1,
        DisconnectCode::Timeout => 2,
        DisconnectCode::AuthenticationFailed => 3,
        DisconnectCode::OwnershipFailed => 4,
        DisconnectCode::IncompatibleProtocol => 5,
        DisconnectCode::IncompatibleSimulation => 6,
        DisconnectCode::IncompatibleBuild => 7,
        DisconnectCode::IncompatibleContent => 8,
        DisconnectCode::InvalidInput => 9,
        DisconnectCode::MalformedTraffic => 10,
        DisconnectCode::RateLimited => 11,
        DisconnectCode::Kicked => 12,
        DisconnectCode::AuthorityLost => 13,
        DisconnectCode::ServerShutdown => 14,
    }
}

fn disconnect_code_from_wire(value: u8) -> Result<DisconnectCode, DecodeError> {
    match value {
        1 => Ok(DisconnectCode::ClientRequested),
        2 => Ok(DisconnectCode::Timeout),
        3 => Ok(DisconnectCode::AuthenticationFailed),
        4 => Ok(DisconnectCode::OwnershipFailed),
        5 => Ok(DisconnectCode::IncompatibleProtocol),
        6 => Ok(DisconnectCode::IncompatibleSimulation),
        7 => Ok(DisconnectCode::IncompatibleBuild),
        8 => Ok(DisconnectCode::IncompatibleContent),
        9 => Ok(DisconnectCode::InvalidInput),
        10 => Ok(DisconnectCode::MalformedTraffic),
        11 => Ok(DisconnectCode::RateLimited),
        12 => Ok(DisconnectCode::Kicked),
        13 => Ok(DisconnectCode::AuthorityLost),
        14 => Ok(DisconnectCode::ServerShutdown),
        _ => Err(DecodeError::InvalidValue {
            field: WireField::DisconnectCode,
            value: u64::from(value),
        }),
    }
}

const fn retry_to_wire(retry: RetryDisposition) -> u8 {
    match retry {
        RetryDisposition::ReturnToLobby => 1,
        RetryDisposition::ReconnectAllowed => 2,
        RetryDisposition::MatchEndedNoContest => 3,
        RetryDisposition::Fatal => 4,
    }
}

fn retry_from_wire(value: u8) -> Result<RetryDisposition, DecodeError> {
    match value {
        1 => Ok(RetryDisposition::ReturnToLobby),
        2 => Ok(RetryDisposition::ReconnectAllowed),
        3 => Ok(RetryDisposition::MatchEndedNoContest),
        4 => Ok(RetryDisposition::Fatal),
        _ => Err(DecodeError::InvalidValue {
            field: WireField::RetryDisposition,
            value: u64::from(value),
        }),
    }
}

struct Writer<'a> {
    output: &'a mut [u8],
    position: usize,
}

impl<'a> Writer<'a> {
    fn new(output: &'a mut [u8]) -> Self {
        Self {
            output,
            position: 0,
        }
    }

    const fn len(&self) -> usize {
        self.position
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), EncodeError> {
        let end = self.position.saturating_add(bytes.len());
        if end > self.output.len() {
            return Err(EncodeError::BufferTooSmall {
                needed: end + PACKET_HEADER_BYTES,
                available: self.output.len() + PACKET_HEADER_BYTES,
            });
        }
        self.output[self.position..end].copy_from_slice(bytes);
        self.position = end;
        Ok(())
    }

    fn write_u8(&mut self, value: u8) -> Result<(), EncodeError> {
        self.write_bytes(&[value])
    }

    fn write_i8(&mut self, value: i8) -> Result<(), EncodeError> {
        self.write_u8(value as u8)
    }

    fn write_u16(&mut self, value: u16) -> Result<(), EncodeError> {
        self.write_bytes(&value.to_be_bytes())
    }

    fn write_u32(&mut self, value: u32) -> Result<(), EncodeError> {
        self.write_bytes(&value.to_be_bytes())
    }

    fn write_u64(&mut self, value: u64) -> Result<(), EncodeError> {
        self.write_bytes(&value.to_be_bytes())
    }
}

struct Reader<'a> {
    input: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    const fn new(input: &'a [u8]) -> Self {
        Self { input, position: 0 }
    }

    const fn remaining(&self) -> usize {
        self.input.len() - self.position
    }

    fn read_bytes(&mut self, length: usize) -> Result<&'a [u8], DecodeError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(DecodeError::Truncated)?;
        if end > self.input.len() {
            return Err(DecodeError::Truncated);
        }
        let bytes = &self.input[self.position..end];
        self.position = end;
        Ok(bytes)
    }

    fn read_array<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        let mut bytes = [0_u8; N];
        bytes.copy_from_slice(self.read_bytes(N)?);
        Ok(bytes)
    }

    fn read_u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.read_bytes(1)?[0])
    }

    fn read_i8(&mut self) -> Result<i8, DecodeError> {
        Ok(self.read_u8()? as i8)
    }

    fn read_u16(&mut self) -> Result<u16, DecodeError> {
        Ok(u16::from_be_bytes(self.read_array()?))
    }

    fn read_u32(&mut self) -> Result<u32, DecodeError> {
        Ok(u32::from_be_bytes(self.read_array()?))
    }

    fn read_u64(&mut self) -> Result<u64, DecodeError> {
        Ok(u64::from_be_bytes(self.read_array()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compatibility() -> CompatibilityId {
        CompatibilityId {
            protocol: ProtocolVersion::new(1).unwrap(),
            simulation: SimulationVersion::new(2).unwrap(),
            replay: ReplayFormatVersion::new(3).unwrap(),
            build: BuildId::new([4; 16]).unwrap(),
            gameplay_content: GameplayContentHash::new([5; 32]).unwrap(),
        }
    }

    fn match_id() -> MatchId {
        MatchId::new([6; 16]).unwrap()
    }

    fn manifest() -> MatchManifest {
        let assignments = std::array::from_fn::<_, MAX_SEATS, _>(|index| SeatAssignment {
            seat: SeatId::new(index as u8).unwrap(),
            fighter: FighterId::new(index as u8).unwrap(),
            owner: SeatOwner::Peer(PeerId::new(20 + index as u64).unwrap()),
        });
        let ownership = SeatOwnership::from_assignments(&assignments).unwrap();
        let slots = std::array::from_fn(|index| FighterSlotConfig {
            occupied: true,
            fighter: FighterId::new(index as u8).unwrap(),
            team: TeamId::new(index as u8).unwrap(),
            character: DefinitionId::new(100 + index as u16).unwrap(),
            style: DefinitionId::new(200 + index as u16).unwrap(),
            equipment: DefinitionId::new(300 + index as u16).unwrap(),
        });
        MatchManifest {
            compatibility: compatibility(),
            manifest_hash: ManifestHash(0x0102_0304_0506_0708),
            match_id: match_id(),
            authority: AuthorityKind::Listen,
            trusted_results: false,
            arena: DefinitionId::new(9).unwrap(),
            rules: DefinitionId::new(10).unwrap(),
            slots,
            ownership,
            master_gameplay_seed: 0x1112_1314_1516_1718,
            rng_scheme_version: 1,
            tick_rate_hz: 60,
            input_delay_ticks: 2,
            rollback_limit_ticks: 12,
            snapshot_history_ticks: 32,
            agreed_start_tick: SimTick(900),
        }
    }

    fn input_frame(tick: u64, seat: u8, sequence: u16) -> InputFrame {
        InputFrame {
            tick: SimTick(tick),
            seat: SeatId::new(seat).unwrap(),
            movement_x: QuantizedAxis::new(20).unwrap(),
            movement_y: QuantizedAxis::new(-20).unwrap(),
            held_buttons: InputButtons::new(InputButtons::LIGHT | InputButtons::GUARD).unwrap(),
            pressed_buttons: InputButtons::new(InputButtons::LIGHT | InputButtons::DASH).unwrap(),
            released_buttons: InputButtons::new(InputButtons::GUARD).unwrap(),
            sequence: InputSequence(sequence),
        }
    }

    fn full_window(tick: u64, seat: u8, sequence: u16) -> SeatInputWindow {
        let frames = std::array::from_fn::<_, MAX_INPUT_FRAMES_PER_WINDOW, _>(|offset| {
            input_frame(
                tick - offset as u64,
                seat,
                sequence.wrapping_sub(offset as u16),
            )
        });
        SeatInputWindow::from_newest_first(&frames).unwrap()
    }

    fn committed_window(
        tick: u64,
        seat: u8,
        fighter: u8,
        source: CommittedInputSource,
    ) -> CommittedSeatInputWindow {
        let records = std::array::from_fn::<_, MAX_INPUT_FRAMES_PER_WINDOW, _>(|offset| {
            CommittedInputRecord {
                frame: input_frame(
                    tick - offset as u64,
                    seat,
                    80_u16.wrapping_sub(offset as u16),
                ),
                fighter: FighterId::new(fighter).unwrap(),
                source,
            }
        });
        CommittedSeatInputWindow::from_newest_first(&records).unwrap()
    }

    fn resync_window(
        tick: u64,
        seat: u8,
        fighter: u8,
        source: CommittedInputSource,
    ) -> CommittedSeatInputWindow {
        let records = std::array::from_fn::<_, MAX_RESYNC_INPUT_TAIL_TICKS, _>(|offset| {
            CommittedInputRecord {
                frame: input_frame(
                    tick - offset as u64,
                    seat,
                    80_u16.wrapping_sub(offset as u16),
                ),
                fighter: FighterId::new(fighter).unwrap(),
                source,
            }
        });
        CommittedSeatInputWindow::from_newest_first(&records).unwrap()
    }

    fn encode(message: &WireMessage) -> (PacketBuffer, usize) {
        let mut packet = [0_u8; MAX_PACKET_BYTES];
        let length = encode_packet(compatibility().protocol, message, &mut packet).unwrap();
        (packet, length)
    }

    fn assert_round_trip(message: WireMessage) {
        let (packet, length) = encode(&message);
        let decoded = decode_packet(&packet[..length], &compatibility()).unwrap();
        assert_eq!(decoded.message, message);
        assert_eq!(decoded.header.kind, message.kind());
        assert_eq!(decoded.header.channel, message.channel());
        assert_eq!(
            usize::from(decoded.header.payload_bytes),
            length - PACKET_HEADER_BYTES
        );
    }

    #[test]
    fn all_supported_messages_round_trip() {
        assert_round_trip(WireMessage::Handshake(Handshake {
            compatibility: compatibility(),
        }));

        let manifest = manifest();
        assert_round_trip(WireMessage::Start(StartMessage::Manifest(manifest)));
        assert_round_trip(WireMessage::Start(StartMessage::ManifestAccepted {
            match_id: manifest.match_id,
            peer_id: PeerId::new(20).unwrap(),
            manifest_hash: manifest.manifest_hash,
        }));
        assert_round_trip(WireMessage::Start(StartMessage::InitialSyncApplied {
            match_id: manifest.match_id,
            peer_id: PeerId::new(20).unwrap(),
            snapshot_tick: SimTick(850),
            snapshot_hash: StateHash(0x5555),
        }));
        assert_round_trip(WireMessage::Start(StartMessage::Ready {
            match_id: manifest.match_id,
            peer_id: PeerId::new(20).unwrap(),
        }));
        assert_round_trip(WireMessage::Start(StartMessage::Countdown {
            match_id: manifest.match_id,
            start_tick: manifest.agreed_start_tick,
        }));

        let input = InputBatch::new(
            match_id(),
            PeerId::new(7).unwrap(),
            &[full_window(100, 0, 50), full_window(100, 1, 90)],
        )
        .unwrap()
        .with_state_baseline_ack(StateBaselineAck {
            tick: SimTick(96),
            hash: StateHash(0xabcd),
        })
        .unwrap();
        assert_round_trip(WireMessage::InputBatch(input));

        let committed = CommittedInputRelay::new(
            match_id(),
            SimTick(100),
            &[
                committed_window(
                    100,
                    0,
                    0,
                    CommittedInputSource::Peer(PeerId::new(7).unwrap()),
                ),
                committed_window(100, 1, 1, CommittedInputSource::AuthorityBot),
                committed_window(100, 2, 2, CommittedInputSource::MissingSubstitute),
            ],
        )
        .unwrap();
        assert_round_trip(WireMessage::CommittedInputRelay(committed));

        let state = StateHashAndAcks::new(
            match_id(),
            SimTick(100),
            StateHash(0x1234),
            &[
                ProcessedInputAck {
                    seat: SeatId::new(0).unwrap(),
                    processed_through: SimTick(98),
                    sequence: InputSequence(50),
                },
                ProcessedInputAck {
                    seat: SeatId::new(1).unwrap(),
                    processed_through: SimTick(99),
                    sequence: InputSequence(90),
                },
            ],
        )
        .unwrap();
        assert_round_trip(WireMessage::StateHashAndAcks(state));

        let base = vec![3_u8; 1_500];
        let mut target = base.clone();
        target[20] = 4;
        target[1_020..1_023].copy_from_slice(&[5, 6, 7]);
        let delta = SnapshotByteDelta::from_canonical_bytes(&base, &target).unwrap();
        let state_delta = StateDeltaAndAcks::new(
            match_id(),
            SimTick(97),
            StateHash(0x1111),
            SimTick(100),
            StateHash(0x2222),
            delta,
            state.as_slice(),
        )
        .unwrap();
        assert_round_trip(WireMessage::StateDeltaAndAcks(state_delta));

        assert_round_trip(WireMessage::ResyncRequest(ResyncRequest {
            match_id: match_id(),
            peer_id: PeerId::new(20).unwrap(),
            reason: ResyncReason::HashMismatch,
            last_confirmed_tick: SimTick(90),
            last_confirmed_hash: StateHash(0x4567),
        }));

        let resync_begin = ResyncBegin {
            match_id: match_id(),
            transfer_id: TransferId::new(8).unwrap(),
            snapshot_tick: SimTick(95),
            snapshot_hash: StateHash(0x5678),
            snapshot_bytes: 4,
            chunk_count: 1,
            recent_input_start: SimTick(91),
            recent_input_end: SimTick(95),
        };
        assert_round_trip(WireMessage::ResyncBegin(resync_begin));

        let (payload, payload_len) = ResyncChunkPayload::from_bytes(&[1, 2, 3, 4]).unwrap();
        let resync = ResyncChunk {
            match_id: match_id(),
            transfer_id: TransferId::new(8).unwrap(),
            snapshot_tick: SimTick(95),
            snapshot_hash: StateHash(0x5678),
            chunk_index: 0,
            chunk_count: 1,
            payload_len,
            payload,
        };
        assert_round_trip(WireMessage::ResyncChunk(resync));
        let input_tail = ResyncInputTail::new(
            &resync_begin,
            &[
                resync_window(
                    95,
                    0,
                    0,
                    CommittedInputSource::Peer(PeerId::new(20).unwrap()),
                ),
                resync_window(95, 1, 1, CommittedInputSource::AuthorityBot),
                resync_window(95, 2, 2, CommittedInputSource::MissingSubstitute),
            ],
        )
        .unwrap();
        assert!(PACKET_HEADER_BYTES + ResyncInputTail::MAX_WIRE_BYTES < MAX_PACKET_BYTES);
        assert_round_trip(WireMessage::ResyncInputTail(input_tail));

        assert_round_trip(WireMessage::ResyncApplied(ResyncApplied {
            match_id: match_id(),
            transfer_id: TransferId::new(8).unwrap(),
            peer_id: PeerId::new(20).unwrap(),
            snapshot_tick: SimTick(95),
            snapshot_hash: StateHash(0x5678),
        }));

        assert_round_trip(WireMessage::ClockProbe(ClockProbe {
            match_id: match_id(),
            peer_id: PeerId::new(20).unwrap(),
            probe_id: ClockProbeId::new(3).unwrap(),
        }));
        assert_round_trip(WireMessage::ClockReply(ClockReply {
            match_id: match_id(),
            peer_id: PeerId::new(20).unwrap(),
            probe_id: ClockProbeId::new(3).unwrap(),
            authority_tick: SimTick(96),
        }));

        assert_round_trip(WireMessage::Disconnect(DisconnectMessage {
            match_id: Some(match_id()),
            code: DisconnectCode::AuthorityLost,
            retry: RetryDisposition::MatchEndedNoContest,
            detail_code: 42,
            last_confirmed_tick: Some(SimTick(100)),
        }));

        assert_round_trip(WireMessage::ResultIdentifier(ResultIdentifier {
            match_id: match_id(),
            result_id: ResultId::new(9).unwrap(),
            final_tick: SimTick(101),
            final_state_hash: StateHash(0x9abc),
        }));
    }

    #[test]
    fn maximum_input_batch_fits_and_max_plus_one_is_rejected_before_frames() {
        let input = InputBatch::new(
            match_id(),
            PeerId::new(7).unwrap(),
            &[
                full_window(100, 0, 100),
                full_window(100, 1, 100),
                full_window(100, 2, 100),
                full_window(100, 3, 100),
            ],
        )
        .unwrap();
        let (mut packet, length) = encode(&WireMessage::InputBatch(input));
        assert!(length < MAX_PACKET_BYTES);

        let window_count_offset = PACKET_HEADER_BYTES + 16 + 8;
        packet[window_count_offset] = MAX_LOCAL_SEATS + 1;
        assert_eq!(
            decode_packet(&packet[..length], &compatibility()),
            Err(DecodeError::LimitExceeded {
                field: WireField::InputWindowCount,
                value: MAX_LOCAL_SEATS as usize + 1,
                maximum: MAX_LOCAL_SEATS as usize,
            })
        );
    }

    #[test]
    fn state_delta_packet_preserves_transport_envelope_headroom() {
        assert!(
            PACKET_HEADER_BYTES + StateDeltaAndAcks::MAX_WIRE_BYTES + 18
                <= crate::network_io::MAX_AFC_DATAGRAM_BYTES
        );

        let base = vec![0_u8; MAX_STATE_DELTA_BYTES - 4];
        let target = vec![1_u8; base.len()];
        let delta = SnapshotByteDelta::from_canonical_bytes(&base, &target).unwrap();
        let message = StateDeltaAndAcks::new(
            match_id(),
            SimTick(1),
            StateHash(1),
            SimTick(2),
            StateHash(2),
            delta,
            &[],
        )
        .unwrap();
        let (_, encoded_len) = encode(&WireMessage::StateDeltaAndAcks(message));
        assert!(encoded_len + 18 <= crate::network_io::MAX_AFC_DATAGRAM_BYTES);
    }

    #[test]
    fn maximum_manifest_is_bounded_and_nested_fields_fail_closed() {
        const AUTHORITY_OFFSET: usize = Handshake::WIRE_BYTES + 8 + 16;
        const TRUSTED_RESULTS_OFFSET: usize = AUTHORITY_OFFSET + 1;
        const SLOTS_OFFSET: usize = TRUSTED_RESULTS_OFFSET + 1 + 2 + 2;
        const OWNERSHIP_COUNT_OFFSET: usize = SLOTS_OFFSET + MAX_FIGHTERS * FIGHTER_SLOT_WIRE_BYTES;
        const FIRST_OWNER_OFFSET: usize = OWNERSHIP_COUNT_OFFSET + 1 + 1 + 1;

        let message = WireMessage::Start(StartMessage::Manifest(manifest()));
        let (packet, length) = encode(&message);
        assert_eq!(length, PACKET_HEADER_BYTES + MATCH_MANIFEST_MAX_WIRE_BYTES);
        assert!(length <= MAX_PACKET_BYTES);
        assert_round_trip(message);

        let mut mutated = packet;
        mutated[PACKET_HEADER_BYTES + OWNERSHIP_COUNT_OFFSET] = MAX_SEATS as u8 + 1;
        assert_eq!(
            decode_packet(&mutated[..length], &compatibility()),
            Err(DecodeError::LimitExceeded {
                field: WireField::SeatOwnershipCount,
                value: MAX_SEATS + 1,
                maximum: MAX_SEATS,
            })
        );

        mutated = packet;
        mutated[PACKET_HEADER_BYTES + AUTHORITY_OFFSET] = 255;
        assert_eq!(
            decode_packet(&mutated[..length], &compatibility()),
            Err(DecodeError::InvalidValue {
                field: WireField::AuthorityKind,
                value: 255,
            })
        );

        mutated = packet;
        mutated[PACKET_HEADER_BYTES + TRUSTED_RESULTS_OFFSET] = 2;
        assert_eq!(
            decode_packet(&mutated[..length], &compatibility()),
            Err(DecodeError::InvalidValue {
                field: WireField::Boolean,
                value: 2,
            })
        );

        mutated = packet;
        mutated[PACKET_HEADER_BYTES + FIRST_OWNER_OFFSET] = 255;
        assert_eq!(
            decode_packet(&mutated[..length], &compatibility()),
            Err(DecodeError::InvalidValue {
                field: WireField::SeatOwner,
                value: 255,
            })
        );

        mutated = packet;
        let second_assignment = PACKET_HEADER_BYTES + OWNERSHIP_COUNT_OFFSET + 1 + 11;
        mutated[second_assignment] = 0;
        mutated[second_assignment + 1] = 0;
        assert_eq!(
            decode_packet(&mutated[..length], &compatibility()),
            Err(DecodeError::InvalidMessage(
                ProtocolValidationError::DuplicateSeat
            ))
        );

        mutated = packet;
        let simulation_version = PACKET_HEADER_BYTES + 2;
        mutated[simulation_version..simulation_version + 2].copy_from_slice(&99_u16.to_be_bytes());
        assert_eq!(
            decode_packet(&mutated[..length], &compatibility()),
            Err(DecodeError::InvalidMessage(
                ProtocolValidationError::SimulationVersionMismatch
            ))
        );
    }

    #[test]
    fn manifest_inactive_slots_must_remain_canonical() {
        let mut candidate = manifest();
        candidate.slots[1..].fill(FighterSlotConfig::default());
        candidate.ownership =
            SeatOwnership::from_assignments(&candidate.ownership.as_slice()[..1]).unwrap();
        let (packet, length) = encode(&WireMessage::Start(StartMessage::Manifest(candidate)));

        let mut mutated = packet;
        let second_slot_fighter = PACKET_HEADER_BYTES
            + Handshake::WIRE_BYTES
            + 8
            + 16
            + 1
            + 1
            + 2
            + 2
            + FIGHTER_SLOT_WIRE_BYTES
            + 1;
        mutated[second_slot_fighter] = 1;
        assert_eq!(
            decode_packet(&mutated[..length], &compatibility()),
            Err(DecodeError::InvalidMessage(
                ProtocolValidationError::NonCanonicalPadding
            ))
        );
    }

    #[test]
    fn every_start_message_truncation_is_rejected() {
        let manifest = manifest();
        let messages = [
            WireMessage::Start(StartMessage::Manifest(manifest)),
            WireMessage::Start(StartMessage::ManifestAccepted {
                match_id: manifest.match_id,
                peer_id: PeerId::new(20).unwrap(),
                manifest_hash: manifest.manifest_hash,
            }),
            WireMessage::Start(StartMessage::InitialSyncApplied {
                match_id: manifest.match_id,
                peer_id: PeerId::new(20).unwrap(),
                snapshot_tick: SimTick(850),
                snapshot_hash: StateHash(1),
            }),
            WireMessage::Start(StartMessage::Ready {
                match_id: manifest.match_id,
                peer_id: PeerId::new(20).unwrap(),
            }),
            WireMessage::Start(StartMessage::Countdown {
                match_id: manifest.match_id,
                start_tick: manifest.agreed_start_tick,
            }),
        ];
        for message in messages {
            let (packet, length) = encode(&message);
            for truncated_length in 0..length {
                assert!(
                    decode_packet(&packet[..truncated_length], &compatibility()).is_err(),
                    "accepted truncated {:?} at {truncated_length}/{length}",
                    message.kind()
                );
            }
        }
    }

    #[test]
    fn maximum_state_ack_count_round_trips_and_max_plus_one_is_rejected() {
        let acks = std::array::from_fn::<_, MAX_SEATS, _>(|seat| ProcessedInputAck {
            seat: SeatId::new(seat as u8).unwrap(),
            processed_through: SimTick(90 + seat as u64),
            sequence: InputSequence(10 + seat as u16),
        });
        let state = StateHashAndAcks::new(match_id(), SimTick(100), StateHash(7), &acks).unwrap();
        let (mut packet, length) = encode(&WireMessage::StateHashAndAcks(state));
        assert!(decode_packet(&packet[..length], &compatibility()).is_ok());

        let ack_count_offset = PACKET_HEADER_BYTES + 16 + 8 + 8;
        packet[ack_count_offset] = MAX_SEATS as u8 + 1;
        assert_eq!(
            decode_packet(&packet[..length], &compatibility()),
            Err(DecodeError::LimitExceeded {
                field: WireField::StateAckCount,
                value: MAX_SEATS + 1,
                maximum: MAX_SEATS,
            })
        );
    }

    #[test]
    fn packet_and_output_limits_are_enforced() {
        let handshake = WireMessage::Handshake(Handshake {
            compatibility: compatibility(),
        });
        let mut too_small = [0_u8; PACKET_HEADER_BYTES - 1];
        assert!(matches!(
            encode_packet(compatibility().protocol, &handshake, &mut too_small),
            Err(EncodeError::BufferTooSmall { .. })
        ));

        let too_large = [0_u8; MAX_PACKET_BYTES + 1];
        assert_eq!(
            decode_packet(&too_large, &compatibility()),
            Err(DecodeError::PacketTooLarge {
                size: MAX_PACKET_BYTES + 1,
                maximum: MAX_PACKET_BYTES,
            })
        );
    }

    #[test]
    fn every_truncation_and_declared_length_mismatch_is_rejected() {
        let message = WireMessage::Handshake(Handshake {
            compatibility: compatibility(),
        });
        let (mut packet, length) = encode(&message);
        for truncated_length in 0..length {
            assert!(decode_packet(&packet[..truncated_length], &compatibility()).is_err());
        }

        packet[PAYLOAD_LENGTH_OFFSET..PACKET_HEADER_BYTES]
            .copy_from_slice(&((length - PACKET_HEADER_BYTES + 1) as u16).to_be_bytes());
        assert_eq!(
            decode_packet(&packet[..length], &compatibility()),
            Err(DecodeError::LengthMismatch {
                declared: length - PACKET_HEADER_BYTES + 1,
                actual: length - PACKET_HEADER_BYTES,
            })
        );
    }

    #[test]
    fn unknown_protocol_channel_kind_and_wrong_channel_are_rejected() {
        let message = WireMessage::Handshake(Handshake {
            compatibility: compatibility(),
        });
        let (packet, length) = encode(&message);

        let mut mutated = packet;
        mutated[PROTOCOL_OFFSET..CHANNEL_OFFSET].copy_from_slice(&99_u16.to_be_bytes());
        assert_eq!(
            decode_packet(&mutated[..length], &compatibility()),
            Err(DecodeError::UnknownProtocol {
                received: 99,
                expected: compatibility().protocol.get(),
            })
        );

        mutated = packet;
        mutated[CHANNEL_OFFSET] = 99;
        assert_eq!(
            decode_packet(&mutated[..length], &compatibility()),
            Err(DecodeError::UnknownChannel(99))
        );

        mutated = packet;
        mutated[KIND_OFFSET] = 99;
        assert_eq!(
            decode_packet(&mutated[..length], &compatibility()),
            Err(DecodeError::UnknownKind(99))
        );

        mutated = packet;
        mutated[CHANNEL_OFFSET] = channel_to_wire(ProtocolChannel::Input);
        assert_eq!(
            decode_packet(&mutated[..length], &compatibility()),
            Err(DecodeError::KindChannelMismatch)
        );

        mutated = packet;
        let simulation_version_offset = PACKET_HEADER_BYTES + 2;
        mutated[simulation_version_offset..simulation_version_offset + 2]
            .copy_from_slice(&99_u16.to_be_bytes());
        assert_eq!(
            decode_packet(&mutated[..length], &compatibility()),
            Err(DecodeError::InvalidMessage(
                ProtocolValidationError::SimulationVersionMismatch
            ))
        );
    }

    #[test]
    fn invalid_axis_button_mask_and_counts_fail_closed() {
        let input = InputBatch::new(
            match_id(),
            PeerId::new(7).unwrap(),
            &[full_window(100, 0, 100)],
        )
        .unwrap();
        let (packet, length) = encode(&WireMessage::InputBatch(input));
        // Header + match + peer + window count + frame count + tick + seat.
        let movement_x_offset = PACKET_HEADER_BYTES + 16 + 8 + 1 + 1 + 8 + 1;

        let mut mutated = packet;
        mutated[movement_x_offset] = i8::MIN as u8;
        assert_eq!(
            decode_packet(&mutated[..length], &compatibility()),
            Err(DecodeError::InvalidMessage(
                ProtocolValidationError::InvalidAxis
            ))
        );

        mutated = packet;
        let buttons_offset = movement_x_offset + 2;
        mutated[buttons_offset..buttons_offset + 2].copy_from_slice(&(1_u16 << 15).to_be_bytes());
        assert_eq!(
            decode_packet(&mutated[..length], &compatibility()),
            Err(DecodeError::InvalidMessage(
                ProtocolValidationError::UnsupportedButtons
            ))
        );

        mutated = packet;
        let pressed_buttons_offset = buttons_offset + 2;
        mutated[pressed_buttons_offset..pressed_buttons_offset + 2]
            .copy_from_slice(&(1_u16 << 15).to_be_bytes());
        assert_eq!(
            decode_packet(&mutated[..length], &compatibility()),
            Err(DecodeError::InvalidMessage(
                ProtocolValidationError::UnsupportedButtons
            ))
        );

        mutated = packet;
        let frame_count_offset = PACKET_HEADER_BYTES + 16 + 8 + 1;
        mutated[frame_count_offset] = MAX_INPUT_FRAMES_PER_WINDOW as u8 + 1;
        assert_eq!(
            decode_packet(&mutated[..length], &compatibility()),
            Err(DecodeError::LimitExceeded {
                field: WireField::InputFrameCount,
                value: MAX_INPUT_FRAMES_PER_WINDOW + 1,
                maximum: MAX_INPUT_FRAMES_PER_WINDOW,
            })
        );

        mutated = packet;
        mutated[PACKET_HEADER_BYTES..PACKET_HEADER_BYTES + 16].fill(0);
        assert_eq!(
            decode_packet(&mutated[..length], &compatibility()),
            Err(DecodeError::InvalidMessage(
                ProtocolValidationError::ZeroIdentifier
            ))
        );
    }

    #[test]
    fn resync_max_payload_round_trips_and_invalid_length_or_padding_is_rejected() {
        let bytes = [0xa5_u8; MAX_RESYNC_CHUNK_BYTES];
        let (payload, payload_len) = ResyncChunkPayload::from_bytes(&bytes).unwrap();
        let message = WireMessage::ResyncChunk(ResyncChunk {
            match_id: match_id(),
            transfer_id: TransferId::new(1).unwrap(),
            snapshot_tick: SimTick(1),
            snapshot_hash: StateHash(2),
            chunk_index: 0,
            chunk_count: 1,
            payload_len,
            payload,
        });
        let (_, length) = encode(&message);
        assert!(length < MAX_PACKET_BYTES);
        assert_round_trip(message);

        let (small_payload, small_len) = ResyncChunkPayload::from_bytes(&[1, 2, 3]).unwrap();
        let small = WireMessage::ResyncChunk(ResyncChunk {
            match_id: match_id(),
            transfer_id: TransferId::new(1).unwrap(),
            snapshot_tick: SimTick(1),
            snapshot_hash: StateHash(2),
            chunk_index: 0,
            chunk_count: 1,
            payload_len: small_len,
            payload: small_payload,
        });
        let (packet, small_packet_len) = encode(&small);
        let payload_len_offset = PACKET_HEADER_BYTES + 16 + 4 + 8 + 8 + 2 + 2;

        let mut mutated = packet;
        mutated[payload_len_offset..payload_len_offset + 2]
            .copy_from_slice(&((MAX_RESYNC_CHUNK_BYTES + 1) as u16).to_be_bytes());
        assert_eq!(
            decode_packet(&mutated[..small_packet_len], &compatibility()),
            Err(DecodeError::LimitExceeded {
                field: WireField::ResyncPayloadLength,
                value: MAX_RESYNC_CHUNK_BYTES + 1,
                maximum: MAX_RESYNC_CHUNK_BYTES,
            })
        );

        mutated = packet;
        mutated[small_packet_len - 1] = 1;
        assert_eq!(
            decode_packet(&mutated[..small_packet_len], &compatibility()),
            Err(DecodeError::InvalidMessage(
                ProtocolValidationError::NonZeroChunkPadding
            ))
        );
    }

    #[test]
    fn disconnect_rejects_invalid_presence_and_enum_values() {
        let message = WireMessage::Disconnect(DisconnectMessage {
            match_id: None,
            code: DisconnectCode::Timeout,
            retry: RetryDisposition::ReconnectAllowed,
            detail_code: 1,
            last_confirmed_tick: None,
        });
        let (packet, length) = encode(&message);

        let mut mutated = packet;
        mutated[PACKET_HEADER_BYTES] = 2;
        assert_eq!(
            decode_packet(&mutated[..length], &compatibility()),
            Err(DecodeError::InvalidValue {
                field: WireField::Boolean,
                value: 2,
            })
        );

        mutated = packet;
        mutated[PACKET_HEADER_BYTES + 1] = 255;
        assert_eq!(
            decode_packet(&mutated[..length], &compatibility()),
            Err(DecodeError::InvalidValue {
                field: WireField::DisconnectCode,
                value: 255,
            })
        );
    }

    #[test]
    fn arbitrary_bytes_never_panic_or_allocate_from_claimed_lengths() {
        let mut bytes = [0_u8; MAX_PACKET_BYTES + 1];
        let mut state = 0x9e37_79b9_u32;
        for iteration in 0..10_000_usize {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let length = (state as usize ^ iteration) % bytes.len();
            for (index, byte) in bytes[..length].iter_mut().enumerate() {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                *byte = (state >> ((index & 3) * 8)) as u8;
            }
            let result = std::panic::catch_unwind(|| {
                let _ = decode_packet(&bytes[..length], &compatibility());
            });
            assert!(
                result.is_ok(),
                "decoder panicked for arbitrary length {length}"
            );
        }
    }

    #[test]
    fn hostile_start_payloads_never_panic_or_trust_claimed_structure() {
        let mut packet = [0_u8; MAX_PACKET_BYTES];
        packet[MAGIC_OFFSET..PROTOCOL_OFFSET].copy_from_slice(&PACKET_MAGIC);
        packet[PROTOCOL_OFFSET..CHANNEL_OFFSET]
            .copy_from_slice(&compatibility().protocol.get().to_be_bytes());
        packet[CHANNEL_OFFSET] = channel_to_wire(ProtocolChannel::Control);
        let kinds = [
            MessageKind::Manifest,
            MessageKind::ManifestAccepted,
            MessageKind::InitialSyncApplied,
            MessageKind::Ready,
            MessageKind::Countdown,
        ];
        let mut state = 0xa341_316c_u32;
        for iteration in 0..5_000_usize {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let payload_len =
                (state as usize ^ iteration) % (MAX_PACKET_BYTES - PACKET_HEADER_BYTES);
            let packet_len = PACKET_HEADER_BYTES + payload_len;
            packet[KIND_OFFSET] = kinds[iteration % kinds.len()] as u8;
            packet[PAYLOAD_LENGTH_OFFSET..PACKET_HEADER_BYTES]
                .copy_from_slice(&(payload_len as u16).to_be_bytes());
            for byte in &mut packet[PACKET_HEADER_BYTES..packet_len] {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                *byte = (state >> 24) as u8;
            }
            let result = std::panic::catch_unwind(|| {
                let _ = decode_packet(&packet[..packet_len], &compatibility());
            });
            assert!(
                result.is_ok(),
                "start decoder panicked for {:?} payload length {payload_len}",
                kinds[iteration % kinds.len()]
            );
        }
    }
}
