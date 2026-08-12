//! Bounded application control protocol for quarantined Steam socket links.
//!
//! `AFCP` is deliberately separate from the unchanged `AFCN` gameplay wire
//! protocol. Every envelope carries a physical connection generation and an
//! application acknowledgement. Steam's reliable send mode protects bytes in
//! transit; the acknowledgement proves that the remote application consumed
//! the frame.

use core::fmt;

use crate::match_config::current_compatibility;
use crate::network_codec::{MAX_PACKET_BYTES, WireMessage, decode_packet, encode_packet};
use crate::network_io::{AfcDatagram, MAX_AFC_DATAGRAM_BYTES};
use crate::network_protocol::{ManifestHash, MatchId, MatchManifest, PeerId, StartMessage};
use crate::online_roster::{FirstReleaseOnlinePolicy, MAX_ONLINE_ROSTER_MEMBERS};
use crate::steam_platform::{
    AdmissionPurpose, MAX_STEAM_AUTH_TICKET_BYTES, SteamLobbyId, SteamUserId,
};

pub const STEAM_CONTROL_MAGIC: [u8; 4] = *b"AFCP";
pub const STEAM_CONTROL_PROTOCOL_VERSION: u8 = 2;
pub const STEAM_LOBBY_SCHEMA_VERSION: u16 = 4;
pub const MAX_STEAM_CONTROL_FRAME_BYTES: usize = MAX_AFC_DATAGRAM_BYTES;

const CONTROL_HEADER_BYTES: usize = 28;
const CONTROL_IDENTITY_BYTES: usize = 24;
const KIND_ACK: u8 = 0;
const KIND_LINK_HELLO: u8 = 1;
const KIND_AUTH_TICKET: u8 = 2;
const KIND_AUTH_ACCEPTED: u8 = 3;
const KIND_MANIFEST_PREPARE: u8 = 4;
const KIND_MANIFEST_ACCEPTED: u8 = 5;
const KIND_MANIFEST_COMMIT: u8 = 6;
const KIND_ABORT: u8 = 7;
const KIND_ROSTER_PREPARE: u8 = 8;
const KIND_ROSTER_ACCEPTED: u8 = 9;
const KIND_ROUTED_AUTH_TICKET: u8 = 10;
const KIND_ROUTED_AUTH_ACCEPTED: u8 = 11;
const KIND_ROSTER_AUTH_COMPLETE: u8 = 12;
const KIND_MANIFEST_COMMIT_ACCEPTED: u8 = 13;
const KIND_GAMEPLAY_ACTIVATE: u8 = 14;
const KIND_GAMEPLAY_ACTIVATED: u8 = 15;
const KIND_SETUP_CANCEL: u8 = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AccountAuthEpoch(u64);

impl AccountAuthEpoch {
    pub const fn new(value: u64) -> Result<Self, SteamControlCodecError> {
        if value == 0 {
            Err(SteamControlCodecError::InvalidPayload)
        } else {
            Ok(Self(value))
        }
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ManifestTransactionId(u64);

impl ManifestTransactionId {
    pub const fn new(value: u64) -> Result<Self, SteamControlCodecError> {
        if value == 0 {
            Err(SteamControlCodecError::InvalidPayload)
        } else {
            Ok(Self(value))
        }
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SteamControlIdentity {
    pub lobby: SteamLobbyId,
    pub sender: SteamUserId,
    pub recipient: SteamUserId,
}

impl SteamControlIdentity {
    pub fn new(
        lobby: SteamLobbyId,
        sender: SteamUserId,
        recipient: SteamUserId,
    ) -> Result<Self, SteamControlCodecError> {
        if sender == recipient {
            return Err(SteamControlCodecError::InvalidIdentity);
        }
        Ok(Self {
            lobby,
            sender,
            recipient,
        })
    }

    pub const fn reverse(self) -> Self {
        Self {
            lobby: self.lobby,
            sender: self.recipient,
            recipient: self.sender,
        }
    }
}

#[derive(PartialEq, Eq)]
pub struct SteamAuthTicketPayload {
    pub identity: SteamControlIdentity,
    pub sender_peer_id: PeerId,
    pub purpose: AdmissionPurpose,
    pub owner_revision: u16,
    pub sender_revision: u16,
    pub match_id: Option<MatchId>,
    ticket_len: u16,
    ticket: [u8; MAX_STEAM_AUTH_TICKET_BYTES],
}

impl SteamAuthTicketPayload {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        identity: SteamControlIdentity,
        sender_peer_id: PeerId,
        purpose: AdmissionPurpose,
        owner_revision: u16,
        sender_revision: u16,
        match_id: Option<MatchId>,
        ticket: &[u8],
    ) -> Result<Self, SteamControlCodecError> {
        sender_peer_id
            .validate()
            .map_err(|_| SteamControlCodecError::InvalidIdentity)?;
        if owner_revision == 0
            || sender_revision == 0
            || ticket.is_empty()
            || ticket.len() > MAX_STEAM_AUTH_TICKET_BYTES
            || matches!(
                (purpose, match_id),
                (AdmissionPurpose::Initial, Some(_)) | (AdmissionPurpose::Reconnect, None)
            )
        {
            return Err(SteamControlCodecError::InvalidPayload);
        }
        if let Some(match_id) = match_id {
            match_id
                .validate()
                .map_err(|_| SteamControlCodecError::InvalidPayload)?;
        }
        let mut retained = [0_u8; MAX_STEAM_AUTH_TICKET_BYTES];
        retained[..ticket.len()].copy_from_slice(ticket);
        Ok(Self {
            identity,
            sender_peer_id,
            purpose,
            owner_revision,
            sender_revision,
            match_id,
            ticket_len: ticket.len() as u16,
            ticket: retained,
        })
    }

    pub fn ticket(&self) -> &[u8] {
        &self.ticket[..usize::from(self.ticket_len)]
    }
}

impl fmt::Debug for SteamAuthTicketPayload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SteamAuthTicketPayload")
            .field("identity", &self.identity)
            .field("sender_peer_id", &self.sender_peer_id)
            .field("purpose", &self.purpose)
            .field("owner_revision", &self.owner_revision)
            .field("sender_revision", &self.sender_revision)
            .field("match_id", &self.match_id)
            .field("ticket_len", &self.ticket_len)
            .field("ticket", &"<redacted>")
            .finish()
    }
}

impl Drop for SteamAuthTicketPayload {
    fn drop(&mut self) {
        self.ticket.fill(0);
        std::hint::black_box(&mut self.ticket);
        self.ticket_len = 0;
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum SteamControlMessage {
    LinkHello {
        identity: SteamControlIdentity,
        lobby_schema: u16,
    },
    AuthTicket(SteamAuthTicketPayload),
    AuthAccepted {
        identity: SteamControlIdentity,
        ticket_sequence: u32,
    },
    RosterPrepare {
        identity: SteamControlIdentity,
        auth_epoch: AccountAuthEpoch,
        roster_hash: u64,
        member_count: u8,
    },
    RosterAccepted {
        identity: SteamControlIdentity,
        auth_epoch: AccountAuthEpoch,
        roster_hash: u64,
        member_count: u8,
    },
    /// A ticket routed over the authority-star physical topology. `identity`
    /// names the physical hop; `ticket.identity` binds the logical issuer and
    /// recipient whose Steam accounts are being mutually authenticated.
    RoutedAuthTicket {
        identity: SteamControlIdentity,
        auth_epoch: AccountAuthEpoch,
        ticket_id: u32,
        ticket: SteamAuthTicketPayload,
    },
    RoutedAuthAccepted {
        identity: SteamControlIdentity,
        auth_epoch: AccountAuthEpoch,
        ticket_id: u32,
        ticket_sender: SteamUserId,
        ticket_recipient: SteamUserId,
    },
    RosterAuthComplete {
        identity: SteamControlIdentity,
        auth_epoch: AccountAuthEpoch,
        roster_hash: u64,
    },
    ManifestPrepare {
        identity: SteamControlIdentity,
        transaction: ManifestTransactionId,
        manifest: MatchManifest,
    },
    ManifestAccepted {
        identity: SteamControlIdentity,
        transaction: ManifestTransactionId,
        manifest_hash: ManifestHash,
    },
    ManifestCommit {
        identity: SteamControlIdentity,
        transaction: ManifestTransactionId,
        manifest_hash: ManifestHash,
    },
    ManifestCommitAccepted {
        identity: SteamControlIdentity,
        transaction: ManifestTransactionId,
        manifest_hash: ManifestHash,
    },
    GameplayActivate {
        identity: SteamControlIdentity,
        transaction: ManifestTransactionId,
        manifest_hash: ManifestHash,
    },
    GameplayActivated {
        identity: SteamControlIdentity,
        transaction: ManifestTransactionId,
        manifest_hash: ManifestHash,
    },
    Abort {
        identity: SteamControlIdentity,
        transaction: Option<ManifestTransactionId>,
        code: u16,
        permanent: bool,
    },
    SetupCancel {
        identity: SteamControlIdentity,
        transaction: ManifestTransactionId,
        code: u16,
    },
}

impl SteamControlMessage {
    pub const fn identity(&self) -> SteamControlIdentity {
        match self {
            Self::LinkHello { identity, .. }
            | Self::AuthAccepted { identity, .. }
            | Self::RosterPrepare { identity, .. }
            | Self::RosterAccepted { identity, .. }
            | Self::RoutedAuthTicket { identity, .. }
            | Self::RoutedAuthAccepted { identity, .. }
            | Self::RosterAuthComplete { identity, .. }
            | Self::ManifestPrepare { identity, .. }
            | Self::ManifestAccepted { identity, .. }
            | Self::ManifestCommit { identity, .. }
            | Self::ManifestCommitAccepted { identity, .. }
            | Self::GameplayActivate { identity, .. }
            | Self::GameplayActivated { identity, .. }
            | Self::SetupCancel { identity, .. }
            | Self::Abort { identity, .. } => *identity,
            Self::AuthTicket(ticket) => ticket.identity,
        }
    }

    pub const fn manifest_transaction(&self) -> Option<ManifestTransactionId> {
        match self {
            Self::ManifestPrepare { transaction, .. }
            | Self::ManifestAccepted { transaction, .. }
            | Self::ManifestCommit { transaction, .. }
            | Self::ManifestCommitAccepted { transaction, .. }
            | Self::GameplayActivate { transaction, .. }
            | Self::GameplayActivated { transaction, .. }
            | Self::SetupCancel { transaction, .. } => Some(*transaction),
            Self::Abort { transaction, .. } => *transaction,
            _ => None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct SteamControlEnvelope {
    pub generation: u32,
    pub sequence: u32,
    pub acknowledgement_generation: u32,
    pub acknowledgement: u32,
    /// Present only on a standalone ACK. Message envelopes obtain their
    /// physical-hop identity from the message itself.
    pub acknowledgement_identity: Option<SteamControlIdentity>,
    pub message: Option<SteamControlMessage>,
}

impl SteamControlEnvelope {
    pub fn message(
        generation: u32,
        sequence: u32,
        acknowledgement: u32,
        message: SteamControlMessage,
    ) -> Result<Self, SteamControlCodecError> {
        if generation == 0 || sequence == 0 {
            return Err(SteamControlCodecError::InvalidEnvelope);
        }
        Self::message_with_ack_generation(
            generation,
            sequence,
            (acknowledgement != 0).then_some(generation),
            acknowledgement,
            message,
        )
    }

    pub fn message_with_ack_generation(
        generation: u32,
        sequence: u32,
        acknowledgement_generation: Option<u32>,
        acknowledgement: u32,
        message: SteamControlMessage,
    ) -> Result<Self, SteamControlCodecError> {
        if generation == 0
            || sequence == 0
            || acknowledgement_generation.is_some_and(|value| value == 0)
            || acknowledgement_generation.is_some() != (acknowledgement != 0)
        {
            return Err(SteamControlCodecError::InvalidEnvelope);
        }
        Ok(Self {
            generation,
            sequence,
            acknowledgement_generation: acknowledgement_generation.unwrap_or(0),
            acknowledgement,
            acknowledgement_identity: None,
            message: Some(message),
        })
    }

    pub fn acknowledgement(
        generation: u32,
        acknowledgement_generation: u32,
        acknowledgement: u32,
        identity: SteamControlIdentity,
    ) -> Result<Self, SteamControlCodecError> {
        if generation == 0 || acknowledgement_generation == 0 || acknowledgement == 0 {
            return Err(SteamControlCodecError::InvalidEnvelope);
        }
        Ok(Self {
            generation,
            sequence: 0,
            acknowledgement_generation,
            acknowledgement,
            acknowledgement_identity: Some(identity),
            message: None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SteamControlCodecError {
    Empty,
    Oversized,
    Truncated,
    InvalidMagic,
    UnsupportedVersion,
    UnknownKind,
    ReservedBits,
    InvalidEnvelope,
    InvalidIdentity,
    InvalidPayload,
    GameplayCodec,
}

impl SteamControlCodecError {
    /// Stable privacy-safe diagnostic code. Never cast enum ordinals.
    pub const fn diagnostic_code(self) -> u16 {
        match self {
            Self::Empty => 301,
            Self::Oversized => 302,
            Self::Truncated => 303,
            Self::InvalidMagic => 304,
            Self::UnsupportedVersion => 305,
            Self::UnknownKind => 306,
            Self::ReservedBits => 307,
            Self::InvalidEnvelope => 308,
            Self::InvalidIdentity => 309,
            Self::InvalidPayload => 310,
            Self::GameplayCodec => 311,
        }
    }
}

impl fmt::Display for SteamControlCodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Steam control codec failure: {self:?}")
    }
}

impl std::error::Error for SteamControlCodecError {}

/// Encoded AFCP storage is always scrubbed on release. Most AFCP frames are not
/// secret, but treating the bounded control buffer uniformly prevents a future
/// ticket-bearing variant from silently bypassing cleanup.
#[derive(PartialEq, Eq)]
pub struct EncodedSteamControlFrame {
    datagram: AfcDatagram,
}

impl EncodedSteamControlFrame {
    pub fn as_slice(&self) -> &[u8] {
        self.datagram.as_slice()
    }

    pub const fn len(&self) -> usize {
        self.datagram.len()
    }

    pub(crate) const fn datagram(&self) -> &AfcDatagram {
        &self.datagram
    }

    #[cfg(test)]
    pub(crate) fn into_datagram(mut self) -> AfcDatagram {
        core::mem::take(&mut self.datagram)
    }

    #[cfg(test)]
    fn zeroize_for_test(&mut self) {
        self.datagram.zeroize();
    }
}

impl fmt::Debug for EncodedSteamControlFrame {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EncodedSteamControlFrame")
            .field("len", &self.len())
            .field("bytes", &"<redacted>")
            .finish()
    }
}

impl Drop for EncodedSteamControlFrame {
    fn drop(&mut self) {
        self.datagram.zeroize();
    }
}

pub fn encode_steam_control(
    envelope: &SteamControlEnvelope,
) -> Result<EncodedSteamControlFrame, SteamControlCodecError> {
    let mut output = [0_u8; MAX_STEAM_CONTROL_FRAME_BYTES];
    let encoded = (|| {
        validate_envelope_shape(envelope)?;
        output[0..4].copy_from_slice(&STEAM_CONTROL_MAGIC);
        output[4] = STEAM_CONTROL_PROTOCOL_VERSION;
        output[6..8].fill(0);
        output[8..12].copy_from_slice(&envelope.generation.to_le_bytes());
        output[12..16].copy_from_slice(&envelope.sequence.to_le_bytes());
        output[16..20].copy_from_slice(&envelope.acknowledgement_generation.to_le_bytes());
        output[20..24].copy_from_slice(&envelope.acknowledgement.to_le_bytes());
        output[26..28].fill(0);

        let payload_len = match envelope.message.as_ref() {
            None => {
                output[5] = KIND_ACK;
                let identity = envelope
                    .acknowledgement_identity
                    .ok_or(SteamControlCodecError::InvalidEnvelope)?;
                write_identity(identity, &mut output[CONTROL_HEADER_BYTES..])?;
                CONTROL_IDENTITY_BYTES
            }
            Some(message) => {
                output[5] = message_kind(message);
                encode_message(message, &mut output[CONTROL_HEADER_BYTES..])?
            }
        };
        output[24..26].copy_from_slice(&(payload_len as u16).to_le_bytes());
        AfcDatagram::try_from_slice(&output[..CONTROL_HEADER_BYTES + payload_len])
            .map(|datagram| EncodedSteamControlFrame { datagram })
            .map_err(|_| SteamControlCodecError::Oversized)
    })();
    output.fill(0);
    std::hint::black_box(&mut output);
    encoded
}

pub fn decode_steam_control(bytes: &[u8]) -> Result<SteamControlEnvelope, SteamControlCodecError> {
    if bytes.is_empty() {
        return Err(SteamControlCodecError::Empty);
    }
    if bytes.len() > MAX_STEAM_CONTROL_FRAME_BYTES {
        return Err(SteamControlCodecError::Oversized);
    }
    if bytes.len() < CONTROL_HEADER_BYTES {
        return Err(SteamControlCodecError::Truncated);
    }
    if bytes[0..4] != STEAM_CONTROL_MAGIC {
        return Err(SteamControlCodecError::InvalidMagic);
    }
    if bytes[4] != STEAM_CONTROL_PROTOCOL_VERSION {
        return Err(SteamControlCodecError::UnsupportedVersion);
    }
    if bytes[6] != 0 || bytes[7] != 0 || bytes[26] != 0 || bytes[27] != 0 {
        return Err(SteamControlCodecError::ReservedBits);
    }
    let generation = read_u32(bytes, 8)?;
    let sequence = read_u32(bytes, 12)?;
    let acknowledgement_generation = read_u32(bytes, 16)?;
    let acknowledgement = read_u32(bytes, 20)?;
    let payload_len = usize::from(read_u16(bytes, 24)?);
    if bytes.len() != CONTROL_HEADER_BYTES + payload_len {
        return Err(SteamControlCodecError::InvalidEnvelope);
    }
    let (message, acknowledgement_identity) = if bytes[5] == KIND_ACK {
        if payload_len != CONTROL_IDENTITY_BYTES || sequence != 0 || acknowledgement == 0 {
            return Err(SteamControlCodecError::InvalidEnvelope);
        }
        (None, Some(read_identity(&bytes[CONTROL_HEADER_BYTES..])?))
    } else {
        if sequence == 0 || payload_len == 0 {
            return Err(SteamControlCodecError::InvalidEnvelope);
        }
        (
            Some(decode_message(bytes[5], &bytes[CONTROL_HEADER_BYTES..])?),
            None,
        )
    };
    let envelope = SteamControlEnvelope {
        generation,
        sequence,
        acknowledgement_generation,
        acknowledgement,
        acknowledgement_identity,
        message,
    };
    validate_envelope_shape(&envelope)?;
    Ok(envelope)
}

fn validate_envelope_shape(envelope: &SteamControlEnvelope) -> Result<(), SteamControlCodecError> {
    if envelope.generation == 0
        || (envelope.message.is_some() && envelope.sequence == 0)
        || (envelope.message.is_some() && envelope.acknowledgement_identity.is_some())
        || ((envelope.acknowledgement == 0) != (envelope.acknowledgement_generation == 0))
        || (envelope.message.is_none()
            && (envelope.sequence != 0
                || envelope.acknowledgement == 0
                || envelope.acknowledgement_identity.is_none()))
    {
        return Err(SteamControlCodecError::InvalidEnvelope);
    }
    Ok(())
}

fn encode_message(
    message: &SteamControlMessage,
    output: &mut [u8],
) -> Result<usize, SteamControlCodecError> {
    let identity = message.identity();
    write_identity(identity, output)?;
    match message {
        SteamControlMessage::LinkHello { lobby_schema, .. } => {
            output[CONTROL_IDENTITY_BYTES..CONTROL_IDENTITY_BYTES + 2]
                .copy_from_slice(&lobby_schema.to_le_bytes());
            Ok(CONTROL_IDENTITY_BYTES + 2)
        }
        SteamControlMessage::AuthTicket(ticket) => encode_auth_ticket_payload(ticket, output),
        SteamControlMessage::AuthAccepted {
            ticket_sequence, ..
        } => {
            if *ticket_sequence == 0 {
                return Err(SteamControlCodecError::InvalidPayload);
            }
            output[24..28].copy_from_slice(&ticket_sequence.to_le_bytes());
            Ok(28)
        }
        SteamControlMessage::RosterPrepare {
            auth_epoch,
            roster_hash,
            member_count,
            ..
        }
        | SteamControlMessage::RosterAccepted {
            auth_epoch,
            roster_hash,
            member_count,
            ..
        } => {
            validate_member_count(*member_count)?;
            output[24..32].copy_from_slice(&auth_epoch.get().to_le_bytes());
            output[32..40].copy_from_slice(&roster_hash.to_le_bytes());
            output[40] = *member_count;
            output[41..48].fill(0);
            Ok(48)
        }
        SteamControlMessage::RoutedAuthTicket {
            identity,
            auth_epoch,
            ticket_id,
            ticket,
        } => {
            if *ticket_id == 0 || ticket.identity.lobby != identity.lobby {
                return Err(SteamControlCodecError::InvalidPayload);
            }
            output[24..32].copy_from_slice(&auth_epoch.get().to_le_bytes());
            output[32..36].copy_from_slice(&ticket_id.to_le_bytes());
            output[36..40].fill(0);
            let ticket_len = encode_auth_ticket_payload(ticket, &mut output[40..])?;
            Ok(40 + ticket_len)
        }
        SteamControlMessage::RoutedAuthAccepted {
            auth_epoch,
            ticket_id,
            ticket_sender,
            ticket_recipient,
            ..
        } => {
            if *ticket_id == 0 || ticket_sender == ticket_recipient {
                return Err(SteamControlCodecError::InvalidPayload);
            }
            output[24..32].copy_from_slice(&auth_epoch.get().to_le_bytes());
            output[32..36].copy_from_slice(&ticket_id.to_le_bytes());
            output[36..44].copy_from_slice(&ticket_sender.get().to_le_bytes());
            output[44..52].copy_from_slice(&ticket_recipient.get().to_le_bytes());
            Ok(52)
        }
        SteamControlMessage::RosterAuthComplete {
            auth_epoch,
            roster_hash,
            ..
        } => {
            output[24..32].copy_from_slice(&auth_epoch.get().to_le_bytes());
            output[32..40].copy_from_slice(&roster_hash.to_le_bytes());
            Ok(40)
        }
        SteamControlMessage::ManifestPrepare {
            transaction,
            manifest,
            ..
        } => {
            manifest
                .validate()
                .map_err(|_| SteamControlCodecError::InvalidPayload)?;
            if !FirstReleaseOnlinePolicy::accepts_manifest(manifest) {
                return Err(SteamControlCodecError::InvalidPayload);
            }
            output[24..32].copy_from_slice(&transaction.get().to_le_bytes());
            let packet_len = encode_packet(
                manifest.compatibility.protocol,
                &WireMessage::Start(StartMessage::Manifest(*manifest)),
                &mut output[32..],
            )
            .map_err(|_| SteamControlCodecError::GameplayCodec)?;
            Ok(32 + packet_len)
        }
        SteamControlMessage::ManifestAccepted {
            transaction,
            manifest_hash,
            ..
        }
        | SteamControlMessage::ManifestCommit {
            transaction,
            manifest_hash,
            ..
        }
        | SteamControlMessage::ManifestCommitAccepted {
            transaction,
            manifest_hash,
            ..
        }
        | SteamControlMessage::GameplayActivate {
            transaction,
            manifest_hash,
            ..
        }
        | SteamControlMessage::GameplayActivated {
            transaction,
            manifest_hash,
            ..
        } => {
            output[24..32].copy_from_slice(&transaction.get().to_le_bytes());
            output[32..40].copy_from_slice(&manifest_hash.0.to_le_bytes());
            Ok(40)
        }
        SteamControlMessage::Abort {
            transaction,
            code,
            permanent,
            ..
        } => {
            output[24..32].copy_from_slice(
                &transaction
                    .map_or(0, ManifestTransactionId::get)
                    .to_le_bytes(),
            );
            output[32..34].copy_from_slice(&code.to_le_bytes());
            output[34] = u8::from(*permanent);
            output[35..40].fill(0);
            Ok(40)
        }
        SteamControlMessage::SetupCancel {
            transaction, code, ..
        } => {
            output[24..32].copy_from_slice(&transaction.get().to_le_bytes());
            output[32..34].copy_from_slice(&code.to_le_bytes());
            output[34..40].fill(0);
            Ok(40)
        }
    }
}

fn encode_auth_ticket_payload(
    ticket: &SteamAuthTicketPayload,
    output: &mut [u8],
) -> Result<usize, SteamControlCodecError> {
    let fixed = CONTROL_IDENTITY_BYTES + 32;
    let len = fixed + ticket.ticket().len();
    if len > output.len() {
        return Err(SteamControlCodecError::Oversized);
    }
    write_identity(ticket.identity, output)?;
    output[24..32].copy_from_slice(&ticket.sender_peer_id.get().to_le_bytes());
    output[32] = match ticket.purpose {
        AdmissionPurpose::Initial => 0,
        AdmissionPurpose::Reconnect => 1,
    };
    output[33] = 0;
    output[34..36].copy_from_slice(&ticket.owner_revision.to_le_bytes());
    output[36..38].copy_from_slice(&ticket.sender_revision.to_le_bytes());
    if let Some(match_id) = ticket.match_id {
        output[38..54].copy_from_slice(match_id.as_bytes());
    } else {
        output[38..54].fill(0);
    }
    output[54..56].copy_from_slice(&(ticket.ticket().len() as u16).to_le_bytes());
    output[fixed..len].copy_from_slice(ticket.ticket());
    Ok(len)
}

fn validate_member_count(member_count: u8) -> Result<(), SteamControlCodecError> {
    if (2..=MAX_ONLINE_ROSTER_MEMBERS as u8).contains(&member_count) {
        Ok(())
    } else {
        Err(SteamControlCodecError::InvalidPayload)
    }
}

fn message_kind(message: &SteamControlMessage) -> u8 {
    match message {
        SteamControlMessage::LinkHello { .. } => KIND_LINK_HELLO,
        SteamControlMessage::AuthTicket(_) => KIND_AUTH_TICKET,
        SteamControlMessage::AuthAccepted { .. } => KIND_AUTH_ACCEPTED,
        SteamControlMessage::RosterPrepare { .. } => KIND_ROSTER_PREPARE,
        SteamControlMessage::RosterAccepted { .. } => KIND_ROSTER_ACCEPTED,
        SteamControlMessage::RoutedAuthTicket { .. } => KIND_ROUTED_AUTH_TICKET,
        SteamControlMessage::RoutedAuthAccepted { .. } => KIND_ROUTED_AUTH_ACCEPTED,
        SteamControlMessage::RosterAuthComplete { .. } => KIND_ROSTER_AUTH_COMPLETE,
        SteamControlMessage::ManifestPrepare { .. } => KIND_MANIFEST_PREPARE,
        SteamControlMessage::ManifestAccepted { .. } => KIND_MANIFEST_ACCEPTED,
        SteamControlMessage::ManifestCommit { .. } => KIND_MANIFEST_COMMIT,
        SteamControlMessage::ManifestCommitAccepted { .. } => KIND_MANIFEST_COMMIT_ACCEPTED,
        SteamControlMessage::GameplayActivate { .. } => KIND_GAMEPLAY_ACTIVATE,
        SteamControlMessage::GameplayActivated { .. } => KIND_GAMEPLAY_ACTIVATED,
        SteamControlMessage::Abort { .. } => KIND_ABORT,
        SteamControlMessage::SetupCancel { .. } => KIND_SETUP_CANCEL,
    }
}

fn decode_message(kind: u8, payload: &[u8]) -> Result<SteamControlMessage, SteamControlCodecError> {
    if payload.len() < CONTROL_IDENTITY_BYTES {
        return Err(SteamControlCodecError::Truncated);
    }
    let identity = read_identity(payload)?;
    match kind {
        KIND_LINK_HELLO if payload.len() == 26 => Ok(SteamControlMessage::LinkHello {
            identity,
            lobby_schema: read_u16(payload, 24)?,
        }),
        KIND_AUTH_TICKET if payload.len() >= 57 => Ok(SteamControlMessage::AuthTicket(
            decode_auth_ticket_payload(payload)?,
        )),
        KIND_AUTH_ACCEPTED if payload.len() == 28 => {
            let ticket_sequence = read_u32(payload, 24)?;
            if ticket_sequence == 0 {
                return Err(SteamControlCodecError::InvalidPayload);
            }
            Ok(SteamControlMessage::AuthAccepted {
                identity,
                ticket_sequence,
            })
        }
        KIND_ROSTER_PREPARE | KIND_ROSTER_ACCEPTED if payload.len() == 48 => {
            if payload[41..48].iter().any(|byte| *byte != 0) {
                return Err(SteamControlCodecError::ReservedBits);
            }
            let auth_epoch = AccountAuthEpoch::new(read_u64(payload, 24)?)?;
            let roster_hash = read_u64(payload, 32)?;
            let member_count = payload[40];
            validate_member_count(member_count)?;
            if kind == KIND_ROSTER_PREPARE {
                Ok(SteamControlMessage::RosterPrepare {
                    identity,
                    auth_epoch,
                    roster_hash,
                    member_count,
                })
            } else {
                Ok(SteamControlMessage::RosterAccepted {
                    identity,
                    auth_epoch,
                    roster_hash,
                    member_count,
                })
            }
        }
        KIND_ROUTED_AUTH_TICKET if payload.len() >= 97 => {
            if payload[36..40].iter().any(|byte| *byte != 0) {
                return Err(SteamControlCodecError::ReservedBits);
            }
            let auth_epoch = AccountAuthEpoch::new(read_u64(payload, 24)?)?;
            let ticket_id = read_u32(payload, 32)?;
            if ticket_id == 0 {
                return Err(SteamControlCodecError::InvalidPayload);
            }
            let ticket = decode_auth_ticket_payload(&payload[40..])?;
            if ticket.identity.lobby != identity.lobby {
                return Err(SteamControlCodecError::InvalidPayload);
            }
            Ok(SteamControlMessage::RoutedAuthTicket {
                identity,
                auth_epoch,
                ticket_id,
                ticket,
            })
        }
        KIND_ROUTED_AUTH_ACCEPTED if payload.len() == 52 => {
            let auth_epoch = AccountAuthEpoch::new(read_u64(payload, 24)?)?;
            let ticket_id = read_u32(payload, 32)?;
            let ticket_sender = SteamUserId::new(read_u64(payload, 36)?)
                .map_err(|_| SteamControlCodecError::InvalidIdentity)?;
            let ticket_recipient = SteamUserId::new(read_u64(payload, 44)?)
                .map_err(|_| SteamControlCodecError::InvalidIdentity)?;
            if ticket_id == 0 || ticket_sender == ticket_recipient {
                return Err(SteamControlCodecError::InvalidPayload);
            }
            Ok(SteamControlMessage::RoutedAuthAccepted {
                identity,
                auth_epoch,
                ticket_id,
                ticket_sender,
                ticket_recipient,
            })
        }
        KIND_ROSTER_AUTH_COMPLETE if payload.len() == 40 => {
            Ok(SteamControlMessage::RosterAuthComplete {
                identity,
                auth_epoch: AccountAuthEpoch::new(read_u64(payload, 24)?)?,
                roster_hash: read_u64(payload, 32)?,
            })
        }
        KIND_MANIFEST_PREPARE if payload.len() > 32 => {
            if payload.len() - 32 > MAX_PACKET_BYTES {
                return Err(SteamControlCodecError::Oversized);
            }
            let transaction = ManifestTransactionId::new(read_u64(payload, 24)?)?;
            let decoded = decode_packet(&payload[32..], &current_compatibility())
                .map_err(|_| SteamControlCodecError::GameplayCodec)?;
            let WireMessage::Start(StartMessage::Manifest(manifest)) = decoded.message else {
                return Err(SteamControlCodecError::InvalidPayload);
            };
            manifest
                .validate()
                .map_err(|_| SteamControlCodecError::InvalidPayload)?;
            if !FirstReleaseOnlinePolicy::accepts_manifest(&manifest) {
                return Err(SteamControlCodecError::InvalidPayload);
            }
            Ok(SteamControlMessage::ManifestPrepare {
                identity,
                transaction,
                manifest,
            })
        }
        KIND_MANIFEST_ACCEPTED if payload.len() == 40 => {
            Ok(SteamControlMessage::ManifestAccepted {
                identity,
                transaction: ManifestTransactionId::new(read_u64(payload, 24)?)?,
                manifest_hash: ManifestHash(read_u64(payload, 32)?),
            })
        }
        KIND_MANIFEST_COMMIT if payload.len() == 40 => Ok(SteamControlMessage::ManifestCommit {
            identity,
            transaction: ManifestTransactionId::new(read_u64(payload, 24)?)?,
            manifest_hash: ManifestHash(read_u64(payload, 32)?),
        }),
        KIND_MANIFEST_COMMIT_ACCEPTED if payload.len() == 40 => {
            Ok(SteamControlMessage::ManifestCommitAccepted {
                identity,
                transaction: ManifestTransactionId::new(read_u64(payload, 24)?)?,
                manifest_hash: ManifestHash(read_u64(payload, 32)?),
            })
        }
        KIND_GAMEPLAY_ACTIVATE if payload.len() == 40 => {
            Ok(SteamControlMessage::GameplayActivate {
                identity,
                transaction: ManifestTransactionId::new(read_u64(payload, 24)?)?,
                manifest_hash: ManifestHash(read_u64(payload, 32)?),
            })
        }
        KIND_GAMEPLAY_ACTIVATED if payload.len() == 40 => {
            Ok(SteamControlMessage::GameplayActivated {
                identity,
                transaction: ManifestTransactionId::new(read_u64(payload, 24)?)?,
                manifest_hash: ManifestHash(read_u64(payload, 32)?),
            })
        }
        KIND_ABORT if payload.len() == 40 => {
            if payload[34] > 1 || payload[35..40].iter().any(|byte| *byte != 0) {
                return Err(SteamControlCodecError::ReservedBits);
            }
            let transaction = match read_u64(payload, 24)? {
                0 => None,
                value => Some(ManifestTransactionId::new(value)?),
            };
            Ok(SteamControlMessage::Abort {
                identity,
                transaction,
                code: read_u16(payload, 32)?,
                permanent: payload[34] == 1,
            })
        }
        KIND_SETUP_CANCEL if payload.len() == 40 => {
            if payload[34..40].iter().any(|byte| *byte != 0) {
                return Err(SteamControlCodecError::ReservedBits);
            }
            Ok(SteamControlMessage::SetupCancel {
                identity,
                transaction: ManifestTransactionId::new(read_u64(payload, 24)?)?,
                code: read_u16(payload, 32)?,
            })
        }
        KIND_ACK => Err(SteamControlCodecError::InvalidEnvelope),
        KIND_LINK_HELLO
        | KIND_AUTH_TICKET
        | KIND_AUTH_ACCEPTED
        | KIND_ROSTER_PREPARE
        | KIND_ROSTER_ACCEPTED
        | KIND_ROUTED_AUTH_TICKET
        | KIND_ROUTED_AUTH_ACCEPTED
        | KIND_ROSTER_AUTH_COMPLETE
        | KIND_MANIFEST_PREPARE
        | KIND_MANIFEST_ACCEPTED
        | KIND_MANIFEST_COMMIT
        | KIND_MANIFEST_COMMIT_ACCEPTED
        | KIND_GAMEPLAY_ACTIVATE
        | KIND_GAMEPLAY_ACTIVATED
        | KIND_ABORT
        | KIND_SETUP_CANCEL => Err(SteamControlCodecError::InvalidPayload),
        _ => Err(SteamControlCodecError::UnknownKind),
    }
}

fn decode_auth_ticket_payload(
    payload: &[u8],
) -> Result<SteamAuthTicketPayload, SteamControlCodecError> {
    if payload.len() < 57 {
        return Err(SteamControlCodecError::Truncated);
    }
    if payload[33] != 0 {
        return Err(SteamControlCodecError::ReservedBits);
    }
    let purpose = match payload[32] {
        0 => AdmissionPurpose::Initial,
        1 => AdmissionPurpose::Reconnect,
        _ => return Err(SteamControlCodecError::InvalidPayload),
    };
    let mut match_id_bytes = [0_u8; 16];
    match_id_bytes.copy_from_slice(&payload[38..54]);
    let match_id = if match_id_bytes.iter().all(|byte| *byte == 0) {
        None
    } else {
        Some(MatchId::new(match_id_bytes).map_err(|_| SteamControlCodecError::InvalidPayload)?)
    };
    let ticket_len = usize::from(read_u16(payload, 54)?);
    if ticket_len == 0
        || ticket_len > MAX_STEAM_AUTH_TICKET_BYTES
        || payload.len() != 56 + ticket_len
    {
        return Err(SteamControlCodecError::InvalidPayload);
    }
    SteamAuthTicketPayload::new(
        read_identity(payload)?,
        PeerId::new(read_u64(payload, 24)?).map_err(|_| SteamControlCodecError::InvalidIdentity)?,
        purpose,
        read_u16(payload, 34)?,
        read_u16(payload, 36)?,
        match_id,
        &payload[56..],
    )
}

fn write_identity(
    identity: SteamControlIdentity,
    output: &mut [u8],
) -> Result<(), SteamControlCodecError> {
    if output.len() < CONTROL_IDENTITY_BYTES || identity.sender == identity.recipient {
        return Err(SteamControlCodecError::InvalidIdentity);
    }
    output[0..8].copy_from_slice(&identity.lobby.get().to_le_bytes());
    output[8..16].copy_from_slice(&identity.sender.get().to_le_bytes());
    output[16..24].copy_from_slice(&identity.recipient.get().to_le_bytes());
    Ok(())
}

fn read_identity(bytes: &[u8]) -> Result<SteamControlIdentity, SteamControlCodecError> {
    SteamControlIdentity::new(
        SteamLobbyId::new(read_u64(bytes, 0)?)
            .map_err(|_| SteamControlCodecError::InvalidIdentity)?,
        SteamUserId::new(read_u64(bytes, 8)?)
            .map_err(|_| SteamControlCodecError::InvalidIdentity)?,
        SteamUserId::new(read_u64(bytes, 16)?)
            .map_err(|_| SteamControlCodecError::InvalidIdentity)?,
    )
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, SteamControlCodecError> {
    let raw = bytes
        .get(offset..offset + 2)
        .ok_or(SteamControlCodecError::Truncated)?;
    Ok(u16::from_le_bytes([raw[0], raw[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, SteamControlCodecError> {
    let raw = bytes
        .get(offset..offset + 4)
        .ok_or(SteamControlCodecError::Truncated)?;
    Ok(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, SteamControlCodecError> {
    let raw = bytes
        .get(offset..offset + 8)
        .ok_or(SteamControlCodecError::Truncated)?;
    Ok(u64::from_le_bytes([
        raw[0], raw[1], raw[2], raw[3], raw[4], raw[5], raw[6], raw[7],
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network_protocol::MatchId;

    fn lobby(value: u64) -> SteamLobbyId {
        SteamLobbyId::new(value).unwrap()
    }

    fn user(value: u64) -> SteamUserId {
        SteamUserId::new(value).unwrap()
    }

    fn identity() -> SteamControlIdentity {
        SteamControlIdentity::new(lobby(10), user(20), user(30)).unwrap()
    }

    fn auth_epoch() -> AccountAuthEpoch {
        AccountAuthEpoch::new(71).unwrap()
    }

    fn transaction() -> ManifestTransactionId {
        ManifestTransactionId::new(81).unwrap()
    }

    fn round_trip(message: SteamControlMessage) -> SteamControlEnvelope {
        let envelope = SteamControlEnvelope::message(7, 9, 8, message).unwrap();
        let encoded = encode_steam_control(&envelope).unwrap();
        decode_steam_control(encoded.as_slice()).unwrap()
    }

    #[test]
    fn every_non_manifest_control_shape_round_trips() {
        assert_eq!(
            round_trip(SteamControlMessage::LinkHello {
                identity: identity(),
                lobby_schema: STEAM_LOBBY_SCHEMA_VERSION,
            }),
            SteamControlEnvelope::message(
                7,
                9,
                8,
                SteamControlMessage::LinkHello {
                    identity: identity(),
                    lobby_schema: STEAM_LOBBY_SCHEMA_VERSION,
                },
            )
            .unwrap()
        );
        let ticket = SteamAuthTicketPayload::new(
            identity(),
            PeerId::new(44).unwrap(),
            AdmissionPurpose::Reconnect,
            2,
            3,
            Some(MatchId::new([7; 16]).unwrap()),
            &[1, 2, 3, 4],
        )
        .unwrap();
        assert!(matches!(
            round_trip(SteamControlMessage::AuthTicket(ticket)).message,
            Some(SteamControlMessage::AuthTicket(ref ticket)) if ticket.ticket() == [1, 2, 3, 4]
        ));
        assert!(matches!(
            round_trip(SteamControlMessage::AuthAccepted {
                identity: identity(),
                ticket_sequence: 12,
            })
            .message,
            Some(SteamControlMessage::AuthAccepted {
                ticket_sequence: 12,
                ..
            })
        ));
        for message in [
            SteamControlMessage::RosterPrepare {
                identity: identity(),
                auth_epoch: auth_epoch(),
                roster_hash: 98,
                member_count: 4,
            },
            SteamControlMessage::RosterAccepted {
                identity: identity(),
                auth_epoch: auth_epoch(),
                roster_hash: 98,
                member_count: 4,
            },
            SteamControlMessage::RoutedAuthAccepted {
                identity: identity(),
                auth_epoch: auth_epoch(),
                ticket_id: 72,
                ticket_sender: user(40),
                ticket_recipient: user(50),
            },
            SteamControlMessage::RosterAuthComplete {
                identity: identity(),
                auth_epoch: auth_epoch(),
                roster_hash: 98,
            },
            SteamControlMessage::ManifestAccepted {
                identity: identity(),
                transaction: transaction(),
                manifest_hash: ManifestHash(99),
            },
            SteamControlMessage::ManifestCommit {
                identity: identity(),
                transaction: transaction(),
                manifest_hash: ManifestHash(99),
            },
            SteamControlMessage::ManifestCommitAccepted {
                identity: identity(),
                transaction: transaction(),
                manifest_hash: ManifestHash(99),
            },
            SteamControlMessage::GameplayActivate {
                identity: identity(),
                transaction: transaction(),
                manifest_hash: ManifestHash(99),
            },
            SteamControlMessage::GameplayActivated {
                identity: identity(),
                transaction: transaction(),
                manifest_hash: ManifestHash(99),
            },
            SteamControlMessage::Abort {
                identity: identity(),
                transaction: Some(transaction()),
                code: 17,
                permanent: true,
            },
            SteamControlMessage::SetupCancel {
                identity: identity(),
                transaction: transaction(),
                code: 18,
            },
        ] {
            assert!(round_trip(message).message.is_some());
        }
        let logical_identity = SteamControlIdentity::new(lobby(10), user(40), user(50)).unwrap();
        let routed_ticket = SteamAuthTicketPayload::new(
            logical_identity,
            PeerId::new(44).unwrap(),
            AdmissionPurpose::Initial,
            2,
            3,
            None,
            &[5, 6, 7, 8],
        )
        .unwrap();
        assert!(matches!(
            round_trip(SteamControlMessage::RoutedAuthTicket {
                identity: identity(),
                auth_epoch: auth_epoch(),
                ticket_id: 73,
                ticket: routed_ticket,
            })
            .message,
            Some(SteamControlMessage::RoutedAuthTicket {
                identity: outer,
                ticket_id: 73,
                ref ticket,
                ..
            }) if outer == identity()
                && ticket.identity == logical_identity
                && ticket.ticket() == [5, 6, 7, 8]
        ));
        let ack = SteamControlEnvelope::acknowledgement(7, 8, 55, identity()).unwrap();
        let encoded = encode_steam_control(&ack).unwrap();
        assert_eq!(decode_steam_control(encoded.as_slice()).unwrap(), ack);
    }

    #[test]
    fn hostile_inputs_are_rejected_without_allocating_ticket_material() {
        let hello = SteamControlEnvelope::message(
            1,
            1,
            0,
            SteamControlMessage::LinkHello {
                identity: identity(),
                lobby_schema: STEAM_LOBBY_SCHEMA_VERSION,
            },
        )
        .unwrap();
        let encoded = encode_steam_control(&hello).unwrap();
        let bytes = encoded.as_slice();
        assert_eq!(
            decode_steam_control(&[]),
            Err(SteamControlCodecError::Empty)
        );
        assert_eq!(
            decode_steam_control(&bytes[..8]),
            Err(SteamControlCodecError::Truncated)
        );
        let mut wrong_magic = bytes.to_vec();
        wrong_magic[0] ^= 0xff;
        assert_eq!(
            decode_steam_control(&wrong_magic),
            Err(SteamControlCodecError::InvalidMagic)
        );
        let mut wrong_version = bytes.to_vec();
        wrong_version[4] = STEAM_CONTROL_PROTOCOL_VERSION + 1;
        assert_eq!(
            decode_steam_control(&wrong_version),
            Err(SteamControlCodecError::UnsupportedVersion)
        );
        let mut reserved = bytes.to_vec();
        reserved[6] = 1;
        assert_eq!(
            decode_steam_control(&reserved),
            Err(SteamControlCodecError::ReservedBits)
        );
        let mut trailing = bytes.to_vec();
        trailing.push(0);
        assert_eq!(
            decode_steam_control(&trailing),
            Err(SteamControlCodecError::InvalidEnvelope)
        );
        let mut unknown = bytes.to_vec();
        unknown[5] = 250;
        assert_eq!(
            decode_steam_control(&unknown),
            Err(SteamControlCodecError::UnknownKind)
        );
        assert_eq!(
            decode_steam_control(&vec![0; MAX_STEAM_CONTROL_FRAME_BYTES + 1]),
            Err(SteamControlCodecError::Oversized)
        );
        let mut mismatched_ack_generation = bytes.to_vec();
        mismatched_ack_generation[16..20].copy_from_slice(&99_u32.to_le_bytes());
        assert_eq!(
            decode_steam_control(&mismatched_ack_generation),
            Err(SteamControlCodecError::InvalidEnvelope)
        );
    }

    #[test]
    fn stable_diagnostic_codes_are_explicit_and_unique() {
        let errors = [
            SteamControlCodecError::Empty,
            SteamControlCodecError::Oversized,
            SteamControlCodecError::Truncated,
            SteamControlCodecError::InvalidMagic,
            SteamControlCodecError::UnsupportedVersion,
            SteamControlCodecError::UnknownKind,
            SteamControlCodecError::ReservedBits,
            SteamControlCodecError::InvalidEnvelope,
            SteamControlCodecError::InvalidIdentity,
            SteamControlCodecError::InvalidPayload,
            SteamControlCodecError::GameplayCodec,
        ];
        let mut codes = errors.map(SteamControlCodecError::diagnostic_code);
        codes.sort_unstable();
        assert!(codes.windows(2).all(|pair| pair[0] != pair[1]));
    }

    #[test]
    fn encoder_never_emits_beyond_the_datagram_ceiling() {
        let ticket = SteamAuthTicketPayload::new(
            identity(),
            PeerId::new(44).unwrap(),
            AdmissionPurpose::Initial,
            2,
            3,
            None,
            &[0x5a; MAX_STEAM_AUTH_TICKET_BYTES],
        )
        .unwrap();
        let envelope =
            SteamControlEnvelope::message(1, 1, 0, SteamControlMessage::AuthTicket(ticket))
                .unwrap();
        let encoded = encode_steam_control(&envelope).unwrap();
        assert!(encoded.len() <= MAX_STEAM_CONTROL_FRAME_BYTES);

        let routed = SteamAuthTicketPayload::new(
            SteamControlIdentity::new(lobby(10), user(40), user(50)).unwrap(),
            PeerId::new(44).unwrap(),
            AdmissionPurpose::Initial,
            2,
            3,
            None,
            &[0x5a; MAX_STEAM_AUTH_TICKET_BYTES],
        )
        .unwrap();
        let envelope = SteamControlEnvelope::message(
            1,
            1,
            0,
            SteamControlMessage::RoutedAuthTicket {
                identity: identity(),
                auth_epoch: auth_epoch(),
                ticket_id: 1,
                ticket: routed,
            },
        )
        .unwrap();
        let encoded = encode_steam_control(&envelope).unwrap();
        assert!(encoded.len() <= MAX_STEAM_CONTROL_FRAME_BYTES);
    }

    #[test]
    fn epochs_transactions_and_roster_sizes_reject_zero_or_out_of_range_values() {
        assert_eq!(
            AccountAuthEpoch::new(0),
            Err(SteamControlCodecError::InvalidPayload)
        );
        assert_eq!(
            ManifestTransactionId::new(0),
            Err(SteamControlCodecError::InvalidPayload)
        );
        for member_count in [0, 1, MAX_ONLINE_ROSTER_MEMBERS as u8 + 1] {
            let envelope = SteamControlEnvelope::message(
                1,
                1,
                0,
                SteamControlMessage::RosterPrepare {
                    identity: identity(),
                    auth_epoch: auth_epoch(),
                    roster_hash: 1,
                    member_count,
                },
            )
            .unwrap();
            assert_eq!(
                encode_steam_control(&envelope),
                Err(SteamControlCodecError::InvalidPayload)
            );
        }
    }

    #[test]
    fn encoded_ticket_control_storage_is_redacted_and_explicitly_zeroizable() {
        let secret = [0xa5, 0x5a, 0xc3, 0x3c];
        let ticket = SteamAuthTicketPayload::new(
            identity(),
            PeerId::new(44).unwrap(),
            AdmissionPurpose::Initial,
            2,
            3,
            None,
            &secret,
        )
        .unwrap();
        let envelope =
            SteamControlEnvelope::message(1, 1, 0, SteamControlMessage::AuthTicket(ticket))
                .unwrap();
        let mut encoded = encode_steam_control(&envelope).unwrap();
        assert!(
            encoded
                .as_slice()
                .windows(secret.len())
                .any(|bytes| bytes == secret)
        );
        assert!(!format!("{encoded:?}").contains("165"));
        encoded.zeroize_for_test();
        assert!(encoded.as_slice().is_empty());
    }
}
