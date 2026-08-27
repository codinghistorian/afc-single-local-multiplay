//! Guest identity and private-room admission tokens for the hosted web service.
//!
//! Tokens use fixed-layout, versioned binary claims and HMAC-SHA-256. They are
//! intentionally independent of AFC wire messages: a transport must complete
//! this admission layer before its endpoint can be attached to an authority.

use core::fmt;
use std::collections::HashMap;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::network_protocol::{MatchId, PeerId, SimTick};
use crate::reconnect::AuthenticatedUserId;

type HmacSha256 = Hmac<Sha256>;

const TOKEN_VERSION: u8 = 1;
const HMAC_BYTES: usize = 32;
const GUEST_MAGIC: [u8; 4] = *b"AFGS";
const JOIN_MAGIC: [u8; 4] = *b"AFJT";
const GUEST_PAYLOAD_BYTES: usize = 65;
const GUEST_TOKEN_BYTES: usize = GUEST_PAYLOAD_BYTES + HMAC_BYTES;
const JOIN_PAYLOAD_BYTES: usize = 114;
const JOIN_TOKEN_BYTES: usize = JOIN_PAYLOAD_BYTES + HMAC_BYTES;
const MAX_ENCODED_TOKEN_BYTES: usize = 512;
pub const DEFAULT_GUEST_SESSION_TTL_SECONDS: u64 = 24 * 60 * 60;
pub const DEFAULT_JOIN_TICKET_TTL_SECONDS: u64 = 30;
pub const DEFAULT_TOKEN_CLOCK_SKEW_SECONDS: u64 = 5;
pub const DEFAULT_REPLAY_CACHE_ENTRIES: usize = 8_192;
pub use crate::web_admission::{ADMISSION_ACCEPTED_FRAME, MAX_ADMISSION_TICKET_BYTES};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WebTokenLifetimes {
    pub guest_session_seconds: u64,
    pub join_ticket_seconds: u64,
    pub maximum_clock_skew_seconds: u64,
}

impl Default for WebTokenLifetimes {
    fn default() -> Self {
        Self {
            guest_session_seconds: DEFAULT_GUEST_SESSION_TTL_SECONDS,
            join_ticket_seconds: DEFAULT_JOIN_TICKET_TTL_SECONDS,
            maximum_clock_skew_seconds: DEFAULT_TOKEN_CLOCK_SKEW_SECONDS,
        }
    }
}

impl WebTokenLifetimes {
    pub fn validate(self) -> Result<(), WebIdentityError> {
        if !(5 * 60..=30 * 24 * 60 * 60).contains(&self.guest_session_seconds)
            || !(5..=120).contains(&self.join_ticket_seconds)
            || self.maximum_clock_skew_seconds > 30
        {
            return Err(WebIdentityError::InvalidConfiguration);
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct WebTokenSigningKey {
    key_id: u32,
    secret: Zeroizing<[u8; 32]>,
}

impl WebTokenSigningKey {
    pub fn new(key_id: u32, secret: [u8; 32]) -> Result<Self, WebIdentityError> {
        if key_id == 0 || secret.iter().all(|byte| *byte == 0) {
            return Err(WebIdentityError::InvalidConfiguration);
        }
        Ok(Self {
            key_id,
            secret: Zeroizing::new(secret),
        })
    }

    pub fn from_base64_url(key_id: u32, encoded: &str) -> Result<Self, WebIdentityError> {
        let decoded = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| WebIdentityError::InvalidConfiguration)?;
        let secret: [u8; 32] = decoded
            .try_into()
            .map_err(|_| WebIdentityError::InvalidConfiguration)?;
        Self::new(key_id, secret)
    }

    pub const fn key_id(&self) -> u32 {
        self.key_id
    }

    fn sign(&self, payload: &[u8]) -> [u8; HMAC_BYTES] {
        let mut mac = HmacSha256::new_from_slice(self.secret.as_ref())
            .expect("HMAC accepts every fixed-size AFC signing key");
        mac.update(payload);
        mac.finalize().into_bytes().into()
    }

    fn verify(&self, payload: &[u8], signature: &[u8]) -> bool {
        let mut mac = HmacSha256::new_from_slice(self.secret.as_ref())
            .expect("HMAC accepts every fixed-size AFC signing key");
        mac.update(payload);
        mac.verify_slice(signature).is_ok()
    }
}

impl fmt::Debug for WebTokenSigningKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebTokenSigningKey")
            .field("key_id", &self.key_id)
            .field("secret", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct WebTokenKeyring {
    current: WebTokenSigningKey,
    previous: Option<WebTokenSigningKey>,
    lifetimes: WebTokenLifetimes,
}

impl WebTokenKeyring {
    pub fn new(
        current: WebTokenSigningKey,
        previous: Option<WebTokenSigningKey>,
        lifetimes: WebTokenLifetimes,
    ) -> Result<Self, WebIdentityError> {
        lifetimes.validate()?;
        if previous
            .as_ref()
            .is_some_and(|previous| previous.key_id == current.key_id)
        {
            return Err(WebIdentityError::InvalidConfiguration);
        }
        Ok(Self {
            current,
            previous,
            lifetimes,
        })
    }

    pub const fn lifetimes(&self) -> WebTokenLifetimes {
        self.lifetimes
    }

    pub fn issue_guest_session(
        &self,
        now_unix_seconds: u64,
    ) -> Result<IssuedGuestSession, WebIdentityError> {
        let guest_id = GuestId(random_nonzero_bytes()?);
        let user_id = random_authenticated_user_id()?;
        let session_nonce = random_nonzero_bytes()?;
        let expires_at = now_unix_seconds
            .checked_add(self.lifetimes.guest_session_seconds)
            .ok_or(WebIdentityError::TimelineOverflow)?;
        let claims = GuestSessionClaims {
            guest_id,
            user_id,
            issued_at_unix_seconds: now_unix_seconds,
            expires_at_unix_seconds: expires_at,
            session_nonce,
        };
        let token = self.encode_guest_session(claims);
        Ok(IssuedGuestSession { claims, token })
    }

    pub fn verify_guest_session(
        &self,
        token: &str,
        now_unix_seconds: u64,
    ) -> Result<GuestSessionClaims, WebIdentityError> {
        let decoded = decode_fixed_token::<GUEST_TOKEN_BYTES>(token)?;
        let key = self.verify_envelope(&decoded, GUEST_MAGIC, GUEST_PAYLOAD_BYTES)?;
        let mut cursor = FixedCursor::new(&decoded[..GUEST_PAYLOAD_BYTES]);
        cursor.expect_bytes(&GUEST_MAGIC)?;
        cursor.expect_u8(TOKEN_VERSION)?;
        cursor.expect_u32(key.key_id)?;
        let issued_at_unix_seconds = cursor.read_u64()?;
        let expires_at_unix_seconds = cursor.read_u64()?;
        let guest_id = GuestId::new(cursor.read_array()?)?;
        let user_id =
            AuthenticatedUserId::new(cursor.read_u64()?).ok_or(WebIdentityError::MalformedToken)?;
        let session_nonce = cursor.read_array()?;
        cursor.finish()?;
        validate_times(
            issued_at_unix_seconds,
            expires_at_unix_seconds,
            now_unix_seconds,
            self.lifetimes.guest_session_seconds,
            self.lifetimes.maximum_clock_skew_seconds,
        )?;
        if session_nonce.iter().all(|byte| *byte == 0) {
            return Err(WebIdentityError::MalformedToken);
        }
        Ok(GuestSessionClaims {
            guest_id,
            user_id,
            issued_at_unix_seconds,
            expires_at_unix_seconds,
            session_nonce,
        })
    }

    pub fn issue_join_ticket(
        &self,
        grant: JoinTicketGrant,
        now_unix_seconds: u64,
    ) -> Result<IssuedJoinTicket, WebIdentityError> {
        grant.validate()?;
        let expires_at = now_unix_seconds
            .checked_add(self.lifetimes.join_ticket_seconds)
            .ok_or(WebIdentityError::TimelineOverflow)?;
        let claims = JoinTicketClaims {
            grant,
            issued_at_unix_seconds: now_unix_seconds,
            expires_at_unix_seconds: expires_at,
            nonce: TicketNonce(random_nonzero_bytes()?),
        };
        let token = self.encode_join_ticket(claims);
        Ok(IssuedJoinTicket { claims, token })
    }

    pub fn verify_join_ticket(
        &self,
        token: &str,
        now_unix_seconds: u64,
    ) -> Result<JoinTicketClaims, WebIdentityError> {
        let decoded = decode_fixed_token::<JOIN_TOKEN_BYTES>(token)?;
        let key = self.verify_envelope(&decoded, JOIN_MAGIC, JOIN_PAYLOAD_BYTES)?;
        let mut cursor = FixedCursor::new(&decoded[..JOIN_PAYLOAD_BYTES]);
        cursor.expect_bytes(&JOIN_MAGIC)?;
        cursor.expect_u8(TOKEN_VERSION)?;
        cursor.expect_u32(key.key_id)?;
        let issued_at_unix_seconds = cursor.read_u64()?;
        let expires_at_unix_seconds = cursor.read_u64()?;
        let guest_id = GuestId::new(cursor.read_array()?)?;
        let room_id = PrivateRoomId::new(cursor.read_array()?)?;
        let match_id =
            MatchId::new(cursor.read_array()?).map_err(|_| WebIdentityError::MalformedToken)?;
        let user_id =
            AuthenticatedUserId::new(cursor.read_u64()?).ok_or(WebIdentityError::MalformedToken)?;
        let peer_id =
            PeerId::new(cursor.read_u64()?).map_err(|_| WebIdentityError::MalformedToken)?;
        let mode = match cursor.read_u8()? {
            0 => JoinTicketMode::Initial,
            1 => JoinTicketMode::Reconnect {
                last_confirmed_tick: SimTick(cursor.read_u64()?),
            },
            _ => return Err(WebIdentityError::MalformedToken),
        };
        if mode == JoinTicketMode::Initial {
            cursor.expect_u64(0)?;
        }
        let nonce = TicketNonce::new(cursor.read_array()?)?;
        cursor.finish()?;
        validate_times(
            issued_at_unix_seconds,
            expires_at_unix_seconds,
            now_unix_seconds,
            self.lifetimes.join_ticket_seconds,
            self.lifetimes.maximum_clock_skew_seconds,
        )?;
        let grant = JoinTicketGrant {
            guest_id,
            user_id,
            room_id,
            match_id,
            peer_id,
            mode,
        };
        grant.validate()?;
        Ok(JoinTicketClaims {
            grant,
            issued_at_unix_seconds,
            expires_at_unix_seconds,
            nonce,
        })
    }

    fn encode_guest_session(&self, claims: GuestSessionClaims) -> String {
        let mut payload = Vec::with_capacity(GUEST_TOKEN_BYTES);
        payload.extend_from_slice(&GUEST_MAGIC);
        payload.push(TOKEN_VERSION);
        payload.extend_from_slice(&self.current.key_id.to_be_bytes());
        payload.extend_from_slice(&claims.issued_at_unix_seconds.to_be_bytes());
        payload.extend_from_slice(&claims.expires_at_unix_seconds.to_be_bytes());
        payload.extend_from_slice(claims.guest_id.as_bytes());
        payload.extend_from_slice(&claims.user_id.get().to_be_bytes());
        payload.extend_from_slice(&claims.session_nonce);
        debug_assert_eq!(payload.len(), GUEST_PAYLOAD_BYTES);
        payload.extend_from_slice(&self.current.sign(&payload));
        URL_SAFE_NO_PAD.encode(payload)
    }

    fn encode_join_ticket(&self, claims: JoinTicketClaims) -> String {
        let mut payload = Vec::with_capacity(JOIN_TOKEN_BYTES);
        payload.extend_from_slice(&JOIN_MAGIC);
        payload.push(TOKEN_VERSION);
        payload.extend_from_slice(&self.current.key_id.to_be_bytes());
        payload.extend_from_slice(&claims.issued_at_unix_seconds.to_be_bytes());
        payload.extend_from_slice(&claims.expires_at_unix_seconds.to_be_bytes());
        payload.extend_from_slice(claims.grant.guest_id.as_bytes());
        payload.extend_from_slice(claims.grant.room_id.as_bytes());
        payload.extend_from_slice(claims.grant.match_id.as_bytes());
        payload.extend_from_slice(&claims.grant.user_id.get().to_be_bytes());
        payload.extend_from_slice(&claims.grant.peer_id.get().to_be_bytes());
        match claims.grant.mode {
            JoinTicketMode::Initial => {
                payload.push(0);
                payload.extend_from_slice(&0_u64.to_be_bytes());
            }
            JoinTicketMode::Reconnect {
                last_confirmed_tick,
            } => {
                payload.push(1);
                payload.extend_from_slice(&last_confirmed_tick.get().to_be_bytes());
            }
        }
        payload.extend_from_slice(claims.nonce.as_bytes());
        debug_assert_eq!(payload.len(), JOIN_PAYLOAD_BYTES);
        payload.extend_from_slice(&self.current.sign(&payload));
        URL_SAFE_NO_PAD.encode(payload)
    }

    fn verify_envelope<'a>(
        &'a self,
        decoded: &[u8],
        magic: [u8; 4],
        payload_bytes: usize,
    ) -> Result<&'a WebTokenSigningKey, WebIdentityError> {
        if decoded.get(..4) != Some(magic.as_slice())
            || decoded.get(4).copied() != Some(TOKEN_VERSION)
        {
            return Err(WebIdentityError::MalformedToken);
        }
        let key_id = u32::from_be_bytes(
            decoded
                .get(5..9)
                .ok_or(WebIdentityError::MalformedToken)?
                .try_into()
                .map_err(|_| WebIdentityError::MalformedToken)?,
        );
        let key = if self.current.key_id == key_id {
            &self.current
        } else if let Some(previous) = self
            .previous
            .as_ref()
            .filter(|previous| previous.key_id == key_id)
        {
            previous
        } else {
            return Err(WebIdentityError::UnknownSigningKey);
        };
        let (payload, signature) = decoded.split_at(payload_bytes);
        if !key.verify(payload, signature) {
            return Err(WebIdentityError::InvalidSignature);
        }
        Ok(key)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GuestId([u8; 16]);

impl GuestId {
    pub fn new(bytes: [u8; 16]) -> Result<Self, WebIdentityError> {
        if bytes.iter().all(|byte| *byte == 0) {
            return Err(WebIdentityError::MalformedToken);
        }
        Ok(Self(bytes))
    }

    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    pub fn encoded(self) -> String {
        URL_SAFE_NO_PAD.encode(self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PrivateRoomId([u8; 16]);

impl PrivateRoomId {
    pub fn random() -> Result<Self, WebIdentityError> {
        Self::new(random_nonzero_bytes()?)
    }

    pub fn new(bytes: [u8; 16]) -> Result<Self, WebIdentityError> {
        if bytes.iter().all(|byte| *byte == 0) {
            return Err(WebIdentityError::MalformedToken);
        }
        Ok(Self(bytes))
    }

    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    pub fn encoded(self) -> String {
        URL_SAFE_NO_PAD.encode(self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TicketNonce([u8; 16]);

impl TicketNonce {
    fn new(bytes: [u8; 16]) -> Result<Self, WebIdentityError> {
        if bytes.iter().all(|byte| *byte == 0) {
            return Err(WebIdentityError::MalformedToken);
        }
        Ok(Self(bytes))
    }

    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GuestSessionClaims {
    pub guest_id: GuestId,
    pub user_id: AuthenticatedUserId,
    pub issued_at_unix_seconds: u64,
    pub expires_at_unix_seconds: u64,
    pub session_nonce: [u8; 16],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssuedGuestSession {
    pub claims: GuestSessionClaims,
    pub token: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinTicketMode {
    Initial,
    Reconnect { last_confirmed_tick: SimTick },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JoinTicketGrant {
    pub guest_id: GuestId,
    pub user_id: AuthenticatedUserId,
    pub room_id: PrivateRoomId,
    pub match_id: MatchId,
    pub peer_id: PeerId,
    pub mode: JoinTicketMode,
}

impl JoinTicketGrant {
    fn validate(self) -> Result<(), WebIdentityError> {
        self.match_id
            .validate()
            .map_err(|_| WebIdentityError::MalformedToken)?;
        self.peer_id
            .validate()
            .map_err(|_| WebIdentityError::MalformedToken)?;
        if self.guest_id.0.iter().all(|byte| *byte == 0)
            || self.room_id.0.iter().all(|byte| *byte == 0)
            || self.user_id.get() == 0
        {
            return Err(WebIdentityError::MalformedToken);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JoinTicketClaims {
    pub grant: JoinTicketGrant,
    pub issued_at_unix_seconds: u64,
    pub expires_at_unix_seconds: u64,
    pub nonce: TicketNonce,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssuedJoinTicket {
    pub claims: JoinTicketClaims,
    pub token: String,
}

#[derive(Debug)]
pub struct TicketReplayGuard {
    used: HashMap<TicketNonce, u64>,
    maximum_entries: usize,
}

impl TicketReplayGuard {
    pub fn new(maximum_entries: usize) -> Result<Self, WebIdentityError> {
        if maximum_entries == 0 || maximum_entries > 1_000_000 {
            return Err(WebIdentityError::InvalidConfiguration);
        }
        Ok(Self {
            used: HashMap::with_capacity(maximum_entries.min(1_024)),
            maximum_entries,
        })
    }

    pub fn consume(
        &mut self,
        claims: JoinTicketClaims,
        now_unix_seconds: u64,
    ) -> Result<(), WebIdentityError> {
        if now_unix_seconds >= claims.expires_at_unix_seconds {
            return Err(WebIdentityError::TokenExpired);
        }
        self.used
            .retain(|_, expires_at| *expires_at > now_unix_seconds);
        if self.used.contains_key(&claims.nonce) {
            return Err(WebIdentityError::TicketReplayed);
        }
        if self.used.len() >= self.maximum_entries {
            return Err(WebIdentityError::ReplayCacheFull);
        }
        self.used
            .insert(claims.nonce, claims.expires_at_unix_seconds);
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.used.len()
    }

    pub fn is_empty(&self) -> bool {
        self.used.is_empty()
    }
}

impl Default for TicketReplayGuard {
    fn default() -> Self {
        Self::new(DEFAULT_REPLAY_CACHE_ENTRIES).expect("default replay capacity is valid")
    }
}

pub fn encode_admission_request(ticket: &str) -> Result<Vec<u8>, WebIdentityError> {
    crate::web_admission::encode_admission_request(ticket)
        .map_err(|_| WebIdentityError::MalformedAdmissionFrame)
}

pub fn decode_admission_request(frame: &[u8]) -> Result<&str, WebIdentityError> {
    crate::web_admission::decode_admission_request(frame)
        .map_err(|_| WebIdentityError::MalformedAdmissionFrame)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WebIdentityError {
    InvalidConfiguration,
    RandomnessUnavailable,
    TimelineOverflow,
    MalformedToken,
    UnknownSigningKey,
    InvalidSignature,
    TokenNotYetValid,
    TokenExpired,
    TokenLifetimeExceeded,
    TicketReplayed,
    ReplayCacheFull,
    MalformedAdmissionFrame,
}

impl fmt::Display for WebIdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "web identity operation failed: {self:?}")
    }
}

impl std::error::Error for WebIdentityError {}

fn random_nonzero_bytes<const N: usize>() -> Result<[u8; N], WebIdentityError> {
    for _ in 0..4 {
        let mut bytes = [0; N];
        getrandom::fill(&mut bytes).map_err(|_| WebIdentityError::RandomnessUnavailable)?;
        if bytes.iter().any(|byte| *byte != 0) {
            return Ok(bytes);
        }
    }
    Err(WebIdentityError::RandomnessUnavailable)
}

fn random_authenticated_user_id() -> Result<AuthenticatedUserId, WebIdentityError> {
    for _ in 0..4 {
        let value = u64::from_be_bytes(random_nonzero_bytes()?);
        if let Some(user_id) = AuthenticatedUserId::new(value) {
            return Ok(user_id);
        }
    }
    Err(WebIdentityError::RandomnessUnavailable)
}

fn decode_fixed_token<const N: usize>(token: &str) -> Result<[u8; N], WebIdentityError> {
    if token.is_empty() || token.len() > MAX_ENCODED_TOKEN_BYTES {
        return Err(WebIdentityError::MalformedToken);
    }
    let decoded = URL_SAFE_NO_PAD
        .decode(token)
        .map_err(|_| WebIdentityError::MalformedToken)?;
    decoded
        .try_into()
        .map_err(|_| WebIdentityError::MalformedToken)
}

fn validate_times(
    issued_at: u64,
    expires_at: u64,
    now: u64,
    maximum_lifetime: u64,
    maximum_clock_skew: u64,
) -> Result<(), WebIdentityError> {
    if issued_at > now.saturating_add(maximum_clock_skew) {
        return Err(WebIdentityError::TokenNotYetValid);
    }
    let lifetime = expires_at
        .checked_sub(issued_at)
        .ok_or(WebIdentityError::MalformedToken)?;
    if lifetime == 0 || lifetime > maximum_lifetime {
        return Err(WebIdentityError::TokenLifetimeExceeded);
    }
    if now >= expires_at {
        return Err(WebIdentityError::TokenExpired);
    }
    Ok(())
}

struct FixedCursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> FixedCursor<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn read_array<const N: usize>(&mut self) -> Result<[u8; N], WebIdentityError> {
        let end = self
            .offset
            .checked_add(N)
            .ok_or(WebIdentityError::MalformedToken)?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(WebIdentityError::MalformedToken)?
            .try_into()
            .map_err(|_| WebIdentityError::MalformedToken)?;
        self.offset = end;
        Ok(value)
    }

    fn read_u8(&mut self) -> Result<u8, WebIdentityError> {
        Ok(self.read_array::<1>()?[0])
    }

    fn read_u32(&mut self) -> Result<u32, WebIdentityError> {
        Ok(u32::from_be_bytes(self.read_array()?))
    }

    fn read_u64(&mut self) -> Result<u64, WebIdentityError> {
        Ok(u64::from_be_bytes(self.read_array()?))
    }

    fn expect_bytes(&mut self, expected: &[u8]) -> Result<(), WebIdentityError> {
        let end = self
            .offset
            .checked_add(expected.len())
            .ok_or(WebIdentityError::MalformedToken)?;
        if self.bytes.get(self.offset..end) != Some(expected) {
            return Err(WebIdentityError::MalformedToken);
        }
        self.offset = end;
        Ok(())
    }

    fn expect_u8(&mut self, expected: u8) -> Result<(), WebIdentityError> {
        if self.read_u8()? != expected {
            return Err(WebIdentityError::MalformedToken);
        }
        Ok(())
    }

    fn expect_u32(&mut self, expected: u32) -> Result<(), WebIdentityError> {
        if self.read_u32()? != expected {
            return Err(WebIdentityError::MalformedToken);
        }
        Ok(())
    }

    fn expect_u64(&mut self, expected: u64) -> Result<(), WebIdentityError> {
        if self.read_u64()? != expected {
            return Err(WebIdentityError::MalformedToken);
        }
        Ok(())
    }

    fn finish(self) -> Result<(), WebIdentityError> {
        if self.offset != self.bytes.len() {
            return Err(WebIdentityError::MalformedToken);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(id: u32, byte: u8) -> WebTokenSigningKey {
        WebTokenSigningKey::new(id, [byte; 32]).unwrap()
    }

    fn keyring() -> WebTokenKeyring {
        WebTokenKeyring::new(key(7, 0x51), None, WebTokenLifetimes::default()).unwrap()
    }

    fn grant(guest: GuestSessionClaims) -> JoinTicketGrant {
        JoinTicketGrant {
            guest_id: guest.guest_id,
            user_id: guest.user_id,
            room_id: PrivateRoomId::new([0x22; 16]).unwrap(),
            match_id: MatchId::new([0x33; 16]).unwrap(),
            peer_id: PeerId::new(44).unwrap(),
            mode: JoinTicketMode::Initial,
        }
    }

    #[test]
    fn guest_sessions_are_signed_fixed_lifetime_bearer_identities() {
        let keyring = keyring();
        let issued = keyring.issue_guest_session(1_000).unwrap();
        let verified = keyring.verify_guest_session(&issued.token, 1_001).unwrap();
        assert_eq!(verified, issued.claims);
        assert_eq!(verified.expires_at_unix_seconds, 87_400);
        assert_eq!(issued.token.len(), 130);
        assert_eq!(
            keyring.verify_guest_session(&issued.token, 87_400),
            Err(WebIdentityError::TokenExpired)
        );
    }

    #[test]
    fn signature_tampering_and_unknown_keys_fail_closed() {
        let keyring = keyring();
        let issued = keyring.issue_guest_session(5_000).unwrap();
        let mut decoded = URL_SAFE_NO_PAD.decode(&issued.token).unwrap();
        *decoded.last_mut().unwrap() ^= 0x80;
        let tampered = URL_SAFE_NO_PAD.encode(decoded);
        assert_eq!(
            keyring.verify_guest_session(&tampered, 5_001),
            Err(WebIdentityError::InvalidSignature)
        );

        let other = WebTokenKeyring::new(key(8, 0x61), None, WebTokenLifetimes::default()).unwrap();
        assert_eq!(
            other.verify_guest_session(&issued.token, 5_001),
            Err(WebIdentityError::UnknownSigningKey)
        );
    }

    #[test]
    fn previous_signing_key_verifies_during_rotation() {
        let old = keyring();
        let session = old.issue_guest_session(7_000).unwrap();
        let rotated = WebTokenKeyring::new(
            key(8, 0x62),
            Some(key(7, 0x51)),
            WebTokenLifetimes::default(),
        )
        .unwrap();
        assert_eq!(
            rotated.verify_guest_session(&session.token, 7_001).unwrap(),
            session.claims
        );
    }

    #[test]
    fn join_tickets_round_trip_every_authority_admission_claim() {
        let keyring = keyring();
        let guest = keyring.issue_guest_session(10_000).unwrap().claims;
        let initial = keyring.issue_join_ticket(grant(guest), 10_010).unwrap();
        assert_eq!(initial.token.len(), 195);
        assert_eq!(
            keyring.verify_join_ticket(&initial.token, 10_011).unwrap(),
            initial.claims
        );

        let reconnect_grant = JoinTicketGrant {
            mode: JoinTicketMode::Reconnect {
                last_confirmed_tick: SimTick(918),
            },
            ..grant(guest)
        };
        let reconnect = keyring.issue_join_ticket(reconnect_grant, 10_012).unwrap();
        assert_eq!(
            keyring
                .verify_join_ticket(&reconnect.token, 10_013)
                .unwrap(),
            reconnect.claims
        );
    }

    #[test]
    fn replay_guard_consumes_a_join_nonce_exactly_once_and_prunes_expiry() {
        let keyring = keyring();
        let guest = keyring.issue_guest_session(20_000).unwrap().claims;
        let first = keyring.issue_join_ticket(grant(guest), 20_000).unwrap();
        let mut guard = TicketReplayGuard::new(1).unwrap();
        guard.consume(first.claims, 20_001).unwrap();
        assert_eq!(
            guard.consume(first.claims, 20_001),
            Err(WebIdentityError::TicketReplayed)
        );
        assert_eq!(
            guard.consume(first.claims, 20_030),
            Err(WebIdentityError::TokenExpired)
        );
        let second = keyring.issue_join_ticket(grant(guest), 20_031).unwrap();
        guard.consume(second.claims, 20_031).unwrap();
        assert_eq!(guard.len(), 1);
    }

    #[test]
    fn admission_frame_is_exact_bounded_and_versioned() {
        let ticket = "abc.DEF-123";
        let frame = encode_admission_request(ticket).unwrap();
        assert_eq!(decode_admission_request(&frame), Ok(ticket));

        let mut trailing = frame.clone();
        trailing.push(0);
        assert_eq!(
            decode_admission_request(&trailing),
            Err(WebIdentityError::MalformedAdmissionFrame)
        );
        assert_eq!(
            encode_admission_request(&"x".repeat(MAX_ADMISSION_TICKET_BYTES + 1)),
            Err(WebIdentityError::MalformedAdmissionFrame)
        );
    }
}
