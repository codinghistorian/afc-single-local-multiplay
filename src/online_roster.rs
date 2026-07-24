//! Authenticated lobby roster to immutable match-manifest construction.
//!
//! Lobby discovery and UI may arrive in any order. This boundary canonicalizes
//! authenticated peers, couch-co-op seats, and loadout declarations before the
//! authority commits a manifest. Once built, the manifest is immutable.

use core::fmt;

use crate::headless::HeadlessMatchConfig;
use crate::match_config::{
    CURRENT_RNG_SCHEME_VERSION, DEFAULT_INPUT_DELAY_TICKS, DEFAULT_ROLLBACK_LIMIT_TICKS,
    DEFAULT_SNAPSHOT_HISTORY_TICKS, canonical_manifest_hash, current_compatibility,
    headless_config_from_manifest_at,
};
use crate::network_protocol::{
    AuthorityKind, DefinitionId, FighterSlotConfig, MAX_FIGHTERS, MAX_LOCAL_SEATS,
    MAX_NORMAL_ROLLBACK_TICKS, MatchId, MatchManifest, PeerId, ProtocolValidationError,
    SIMULATION_HZ, SeatAssignment, SeatId, SeatOwner, SeatOwnership, TeamId,
};
use crate::network_quality::{MAX_CALIBRATED_INPUT_DELAY_TICKS, MIN_CALIBRATED_INPUT_DELAY_TICKS};
use crate::reconnect::AuthenticatedUserId;
use crate::simulation::SimTick;

pub const MAX_ONLINE_ROSTER_MEMBERS: usize = MAX_FIGHTERS;
pub const ONLINE_MEMBER_DECLARATION_VERSION: u8 = 1;
pub const MAX_ONLINE_MEMBER_DECLARATION_BYTES: usize = 64;

/// Shipping capability boundary for the first player-facing Steam release.
///
/// Public discovery, hosted dedicated authority, ranked play, and trusted
/// results are deliberately separate future gates. Keeping these values beside
/// the immutable manifest builder gives every application path one auditable
/// policy instead of relying on UI labels or caller discipline.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FirstReleaseOnlinePolicy;

impl FirstReleaseOnlinePolicy {
    pub const PUBLIC_DISCOVERY_ENABLED: bool = false;
    pub const HOSTED_DEDICATED_ENABLED: bool = false;
    pub const RANKED_PLAY_ENABLED: bool = false;
    pub const TRUSTED_RESULTS_ENABLED: bool = false;

    pub const fn accepts_options(options: &OnlineManifestOptions) -> bool {
        matches!(options.authority, AuthorityKind::Listen)
            && options.authority_peer.is_some()
            && !options.trusted_results
            && options.input_delay_ticks >= MIN_CALIBRATED_INPUT_DELAY_TICKS
            && options.input_delay_ticks <= MAX_CALIBRATED_INPUT_DELAY_TICKS
            && options.rollback_limit_ticks == MAX_NORMAL_ROLLBACK_TICKS
    }

    pub const fn accepts_manifest(manifest: &MatchManifest) -> bool {
        matches!(manifest.authority, AuthorityKind::Listen)
            && !manifest.trusted_results
            && manifest.input_delay_ticks >= MIN_CALIBRATED_INPUT_DELAY_TICKS
            && manifest.input_delay_ticks <= MAX_CALIBRATED_INPUT_DELAY_TICKS
            && manifest.rollback_limit_ticks == MAX_NORMAL_ROLLBACK_TICKS
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OnlineSeatSelection {
    pub team: TeamId,
    pub character: DefinitionId,
    pub style: DefinitionId,
    pub equipment: DefinitionId,
}

impl OnlineSeatSelection {
    pub fn validate(self) -> Result<(), ProtocolValidationError> {
        self.team.validate()?;
        self.character.validate()?;
        self.style.validate()?;
        self.equipment.validate()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OnlineRosterMember {
    pub peer_id: PeerId,
    pub authenticated_user: AuthenticatedUserId,
    pub revision: u16,
    pub ready: bool,
    seat_count: u8,
    seats: [OnlineSeatSelection; MAX_FIGHTERS],
}

impl OnlineRosterMember {
    pub fn new(
        peer_id: PeerId,
        authenticated_user: AuthenticatedUserId,
        revision: u16,
        ready: bool,
        seats: &[OnlineSeatSelection],
    ) -> Result<Self, OnlineRosterError> {
        peer_id.validate()?;
        if authenticated_user.get() == 0
            || revision == 0
            || seats.is_empty()
            || seats.len() > usize::from(MAX_LOCAL_SEATS)
        {
            return Err(OnlineRosterError::InvalidDeclaration);
        }
        for selection in seats {
            selection.validate()?;
        }
        let mut retained = [OnlineSeatSelection::default(); MAX_FIGHTERS];
        retained[..seats.len()].copy_from_slice(seats);
        Ok(Self {
            peer_id,
            authenticated_user,
            revision,
            ready,
            seat_count: seats.len() as u8,
            seats: retained,
        })
    }

    pub const fn seat_count(self) -> usize {
        self.seat_count as usize
    }

    pub fn seats(&self) -> &[OnlineSeatSelection] {
        &self.seats[..self.seat_count()]
    }

    pub fn replace_declaration(
        &mut self,
        revision: u16,
        ready: bool,
        seats: &[OnlineSeatSelection],
    ) -> Result<(), OnlineRosterError> {
        if revision <= self.revision {
            return Err(OnlineRosterError::StaleRevision);
        }
        let replacement = Self::new(
            self.peer_id,
            self.authenticated_user,
            revision,
            ready,
            seats,
        )?;
        *self = replacement;
        Ok(())
    }
}

/// Canonical lowercase-hex member declaration suitable for one bounded Steam
/// member-metadata value. Identity and readiness remain separately validated by
/// the platform layer; this payload carries only revision and seat loadouts.
pub fn encode_member_declaration(member: &OnlineRosterMember) -> String {
    let mut bytes = [0_u8; 4 + MAX_FIGHTERS * 7];
    bytes[0] = ONLINE_MEMBER_DECLARATION_VERSION;
    bytes[1..3].copy_from_slice(&member.revision.to_le_bytes());
    bytes[3] = member.seat_count;
    let mut offset = 4;
    for selection in member.seats() {
        bytes[offset] = selection.team.get();
        bytes[offset + 1..offset + 3].copy_from_slice(&selection.character.get().to_le_bytes());
        bytes[offset + 3..offset + 5].copy_from_slice(&selection.style.get().to_le_bytes());
        bytes[offset + 5..offset + 7].copy_from_slice(&selection.equipment.get().to_le_bytes());
        offset += 7;
    }
    encode_lower_hex(&bytes[..offset])
}

pub fn decode_member_declaration(
    peer_id: PeerId,
    authenticated_user: AuthenticatedUserId,
    ready: bool,
    encoded: &str,
) -> Result<OnlineRosterMember, OnlineRosterError> {
    let decoded = parse_member_declaration(encoded)?;
    OnlineRosterMember::new(
        peer_id,
        authenticated_user,
        decoded.revision,
        ready,
        &decoded.seats[..usize::from(decoded.seat_count)],
    )
}

/// Identity-free summary used by the Steam metadata layer before the transport
/// has assigned a protocol peer ID. Full identity binding still happens through
/// [`decode_member_declaration`] after authentication.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OnlineMemberDeclarationSummary {
    pub revision: u16,
    pub seat_count: u8,
}

/// Validates the complete bounded declaration, including every gameplay
/// definition ID, without inventing an unauthenticated peer identity.
pub fn validate_member_declaration(
    encoded: &str,
) -> Result<OnlineMemberDeclarationSummary, OnlineRosterError> {
    let decoded = parse_member_declaration(encoded)?;
    Ok(OnlineMemberDeclarationSummary {
        revision: decoded.revision,
        seat_count: decoded.seat_count,
    })
}

struct DecodedOnlineMemberDeclaration {
    revision: u16,
    seat_count: u8,
    seats: [OnlineSeatSelection; MAX_FIGHTERS],
}

fn parse_member_declaration(
    encoded: &str,
) -> Result<DecodedOnlineMemberDeclaration, OnlineRosterError> {
    if encoded.is_empty()
        || encoded.len() > MAX_ONLINE_MEMBER_DECLARATION_BYTES
        || !encoded.len().is_multiple_of(2)
    {
        return Err(OnlineRosterError::InvalidDeclaration);
    }
    let mut bytes = [0_u8; 4 + MAX_FIGHTERS * 7];
    let byte_len = encoded.len() / 2;
    if byte_len > bytes.len() {
        return Err(OnlineRosterError::InvalidDeclaration);
    }
    for (index, byte) in bytes[..byte_len].iter_mut().enumerate() {
        let offset = index * 2;
        *byte = (decode_lower_hex_nibble(encoded.as_bytes()[offset])? << 4)
            | decode_lower_hex_nibble(encoded.as_bytes()[offset + 1])?;
    }
    if byte_len < 4 || bytes[0] != ONLINE_MEMBER_DECLARATION_VERSION {
        return Err(OnlineRosterError::InvalidDeclaration);
    }
    let revision = u16::from_le_bytes([bytes[1], bytes[2]]);
    let seat_count = usize::from(bytes[3]);
    if seat_count == 0
        || seat_count > usize::from(MAX_LOCAL_SEATS)
        || byte_len != 4 + seat_count * 7
    {
        return Err(OnlineRosterError::InvalidDeclaration);
    }
    let mut seats = [OnlineSeatSelection::default(); MAX_FIGHTERS];
    let mut offset = 4;
    for selection in &mut seats[..seat_count] {
        *selection = OnlineSeatSelection {
            team: TeamId::new(bytes[offset])?,
            character: DefinitionId::new(u16::from_le_bytes([
                bytes[offset + 1],
                bytes[offset + 2],
            ]))?,
            style: DefinitionId::new(u16::from_le_bytes([bytes[offset + 3], bytes[offset + 4]]))?,
            equipment: DefinitionId::new(u16::from_le_bytes([
                bytes[offset + 5],
                bytes[offset + 6],
            ]))?,
        };
        offset += 7;
    }
    if revision == 0 {
        return Err(OnlineRosterError::InvalidDeclaration);
    }
    Ok(DecodedOnlineMemberDeclaration {
        revision,
        seat_count: seat_count as u8,
        seats,
    })
}

fn encode_lower_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[usize::from(byte >> 4)] as char);
        output.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    output
}

fn decode_lower_hex_nibble(byte: u8) -> Result<u8, OnlineRosterError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(OnlineRosterError::InvalidDeclaration),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OnlineManifestOptions {
    pub match_id: MatchId,
    pub authority: AuthorityKind,
    pub authority_peer: Option<PeerId>,
    pub trusted_results: bool,
    pub arena: DefinitionId,
    pub rules: DefinitionId,
    pub master_gameplay_seed: u64,
    pub agreed_start_tick: SimTick,
    pub input_delay_ticks: u8,
    pub rollback_limit_ticks: u8,
    pub snapshot_history_ticks: u8,
}

impl OnlineManifestOptions {
    pub fn casual_listen(
        match_id: MatchId,
        authority_peer: PeerId,
        arena: DefinitionId,
        rules: DefinitionId,
        master_gameplay_seed: u64,
        agreed_start_tick: SimTick,
    ) -> Self {
        Self {
            match_id,
            authority: AuthorityKind::Listen,
            authority_peer: Some(authority_peer),
            trusted_results: false,
            arena,
            rules,
            master_gameplay_seed,
            agreed_start_tick,
            input_delay_ticks: DEFAULT_INPUT_DELAY_TICKS,
            rollback_limit_ticks: DEFAULT_ROLLBACK_LIMIT_TICKS,
            snapshot_history_ticks: DEFAULT_SNAPSHOT_HISTORY_TICKS,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OnlineRosterError {
    Protocol(ProtocolValidationError),
    InvalidDeclaration,
    DuplicatePeer(PeerId),
    DuplicateAuthenticatedUser(AuthenticatedUserId),
    UnknownPeer(PeerId),
    StaleRevision,
    Capacity,
    SeatCapacity,
    NotReady(PeerId),
    MissingAuthorityPeer,
    InvalidAuthorityPeer,
    MatchConfig(crate::match_config::MatchConfigError),
}

impl fmt::Display for OnlineRosterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid online roster operation: {self:?}")
    }
}

impl std::error::Error for OnlineRosterError {}

impl From<ProtocolValidationError> for OnlineRosterError {
    fn from(value: ProtocolValidationError) -> Self {
        Self::Protocol(value)
    }
}

impl From<crate::match_config::MatchConfigError> for OnlineRosterError {
    fn from(value: crate::match_config::MatchConfigError) -> Self {
        Self::MatchConfig(value)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OnlineRosterMetrics {
    pub declarations_accepted: u64,
    pub declarations_replaced: u64,
    pub declarations_rejected: u64,
    pub members_removed: u64,
    pub manifests_built: u64,
}

/// Fixed-capacity authenticated roster. Slots are storage only; manifest order
/// is canonicalized independently from arrival order.
pub struct OnlineRoster {
    members: [Option<OnlineRosterMember>; MAX_ONLINE_ROSTER_MEMBERS],
    len: usize,
    metrics: OnlineRosterMetrics,
}

impl Default for OnlineRoster {
    fn default() -> Self {
        Self {
            members: [None; MAX_ONLINE_ROSTER_MEMBERS],
            len: 0,
            metrics: OnlineRosterMetrics::default(),
        }
    }
}

impl OnlineRoster {
    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn metrics(&self) -> OnlineRosterMetrics {
        self.metrics
    }

    pub fn member(&self, peer_id: PeerId) -> Option<&OnlineRosterMember> {
        self.members
            .iter()
            .flatten()
            .find(|member| member.peer_id == peer_id)
    }

    pub fn total_seats(&self) -> usize {
        self.members
            .iter()
            .flatten()
            .map(|member| member.seat_count())
            .sum()
    }

    pub fn upsert(&mut self, declaration: OnlineRosterMember) -> Result<(), OnlineRosterError> {
        if let Some(index) = self
            .members
            .iter()
            .position(|member| member.is_some_and(|member| member.peer_id == declaration.peer_id))
        {
            let existing = self.members[index].expect("located member exists");
            if existing.authenticated_user != declaration.authenticated_user {
                self.metrics.declarations_rejected =
                    self.metrics.declarations_rejected.saturating_add(1);
                return Err(OnlineRosterError::DuplicatePeer(declaration.peer_id));
            }
            if declaration.revision <= existing.revision {
                self.metrics.declarations_rejected =
                    self.metrics.declarations_rejected.saturating_add(1);
                return Err(OnlineRosterError::StaleRevision);
            }
            let seats_after = self
                .total_seats()
                .saturating_sub(existing.seat_count())
                .saturating_add(declaration.seat_count());
            if seats_after > MAX_FIGHTERS {
                self.metrics.declarations_rejected =
                    self.metrics.declarations_rejected.saturating_add(1);
                return Err(OnlineRosterError::SeatCapacity);
            }
            self.members[index] = Some(declaration);
            self.metrics.declarations_replaced =
                self.metrics.declarations_replaced.saturating_add(1);
            return Ok(());
        }
        if self
            .members
            .iter()
            .flatten()
            .any(|member| member.authenticated_user == declaration.authenticated_user)
        {
            self.metrics.declarations_rejected =
                self.metrics.declarations_rejected.saturating_add(1);
            return Err(OnlineRosterError::DuplicateAuthenticatedUser(
                declaration.authenticated_user,
            ));
        }
        if self.total_seats().saturating_add(declaration.seat_count()) > MAX_FIGHTERS {
            self.metrics.declarations_rejected =
                self.metrics.declarations_rejected.saturating_add(1);
            return Err(OnlineRosterError::SeatCapacity);
        }
        let Some(slot) = self.members.iter_mut().find(|slot| slot.is_none()) else {
            self.metrics.declarations_rejected =
                self.metrics.declarations_rejected.saturating_add(1);
            return Err(OnlineRosterError::Capacity);
        };
        *slot = Some(declaration);
        self.len += 1;
        self.metrics.declarations_accepted = self.metrics.declarations_accepted.saturating_add(1);
        Ok(())
    }

    pub fn remove(&mut self, peer_id: PeerId) -> Result<OnlineRosterMember, OnlineRosterError> {
        let Some(slot) = self
            .members
            .iter_mut()
            .find(|slot| slot.is_some_and(|member| member.peer_id == peer_id))
        else {
            return Err(OnlineRosterError::UnknownPeer(peer_id));
        };
        let member = slot.take().expect("located member exists");
        self.len = self.len.saturating_sub(1);
        self.metrics.members_removed = self.metrics.members_removed.saturating_add(1);
        Ok(member)
    }

    pub fn build_headless_config(
        &mut self,
        options: OnlineManifestOptions,
        now: SimTick,
    ) -> Result<HeadlessMatchConfig, OnlineRosterError> {
        options.match_id.validate()?;
        options.arena.validate()?;
        options.rules.validate()?;
        if self.len == 0 || self.total_seats() == 0 {
            return Err(OnlineRosterError::InvalidDeclaration);
        }
        for member in self.members.iter().flatten() {
            if !member.ready {
                return Err(OnlineRosterError::NotReady(member.peer_id));
            }
        }
        match options.authority {
            AuthorityKind::Listen => {
                let authority = options
                    .authority_peer
                    .ok_or(OnlineRosterError::MissingAuthorityPeer)?;
                if self.member(authority).is_none() {
                    return Err(OnlineRosterError::InvalidAuthorityPeer);
                }
            }
            AuthorityKind::Dedicated => {
                if options.authority_peer.is_some() {
                    return Err(OnlineRosterError::InvalidAuthorityPeer);
                }
            }
            AuthorityKind::Offline => return Err(OnlineRosterError::InvalidAuthorityPeer),
        }

        let mut order = [None; MAX_ONLINE_ROSTER_MEMBERS];
        let mut order_len = 0;
        for (index, member) in self.members.iter().enumerate() {
            if member.is_some() {
                order[order_len] = Some(index);
                order_len += 1;
            }
        }
        // At most four entries: insertion sort keeps this allocation-free and
        // makes arrival/callback order irrelevant. A listen authority sorts
        // first for stable local-seat UX; all other peers sort by PeerId.
        for index in 1..order_len {
            let candidate = order[index].expect("dense order prefix");
            let candidate_member = self.members[candidate].expect("order points to member");
            let mut position = index;
            while position > 0 {
                let previous = order[position - 1].expect("dense order prefix");
                let previous_member = self.members[previous].expect("order points to member");
                if roster_sort_key(previous_member, options.authority_peer)
                    <= roster_sort_key(candidate_member, options.authority_peer)
                {
                    break;
                }
                order[position] = order[position - 1];
                position -= 1;
            }
            order[position] = Some(candidate);
        }

        let mut slots = [FighterSlotConfig::default(); MAX_FIGHTERS];
        let mut assignments = [SeatAssignment::default(); MAX_FIGHTERS];
        let mut fighter_index = 0;
        for member_index in order[..order_len].iter().flatten().copied() {
            let member = self.members[member_index].expect("order points to member");
            for selection in member.seats() {
                let fighter = crate::determinism::FighterId::from_index(fighter_index)
                    .expect("roster seat capacity is bounded");
                let seat = SeatId::new(fighter_index as u8)?;
                slots[fighter_index] = FighterSlotConfig {
                    occupied: true,
                    fighter,
                    team: selection.team,
                    character: selection.character,
                    style: selection.style,
                    equipment: selection.equipment,
                };
                assignments[fighter_index] = SeatAssignment {
                    seat,
                    fighter,
                    owner: SeatOwner::Peer(member.peer_id),
                };
                fighter_index += 1;
            }
        }
        let ownership = SeatOwnership::from_assignments(&assignments[..fighter_index])?;
        let mut manifest = MatchManifest {
            compatibility: current_compatibility(),
            manifest_hash: crate::network_protocol::ManifestHash(0),
            match_id: options.match_id,
            authority: options.authority,
            trusted_results: options.trusted_results,
            arena: options.arena,
            rules: options.rules,
            slots,
            ownership,
            master_gameplay_seed: options.master_gameplay_seed,
            rng_scheme_version: CURRENT_RNG_SCHEME_VERSION,
            tick_rate_hz: SIMULATION_HZ,
            input_delay_ticks: options.input_delay_ticks,
            rollback_limit_ticks: options.rollback_limit_ticks,
            snapshot_history_ticks: options.snapshot_history_ticks,
            agreed_start_tick: options.agreed_start_tick,
        };
        manifest.manifest_hash = canonical_manifest_hash(&manifest);
        let config = headless_config_from_manifest_at(manifest, now)?;
        self.metrics.manifests_built = self.metrics.manifests_built.saturating_add(1);
        Ok(config)
    }
}

fn roster_sort_key(member: OnlineRosterMember, authority_peer: Option<PeerId>) -> (u8, u64) {
    (
        u8::from(authority_peer != Some(member.peer_id)),
        member.peer_id.get(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(value: u64) -> PeerId {
        PeerId::new(value).unwrap()
    }

    fn user(value: u64) -> AuthenticatedUserId {
        AuthenticatedUserId::new(value).unwrap()
    }

    fn selection(character: u16, team: u8) -> OnlineSeatSelection {
        OnlineSeatSelection {
            team: TeamId::new(team).unwrap(),
            character: DefinitionId::new(character).unwrap(),
            style: DefinitionId::new(0).unwrap(),
            equipment: DefinitionId::new(0).unwrap(),
        }
    }

    fn options(authority_peer: PeerId) -> OnlineManifestOptions {
        OnlineManifestOptions::casual_listen(
            MatchId::new(*b"online-roster-01").unwrap(),
            authority_peer,
            DefinitionId::new(0).unwrap(),
            DefinitionId::new(1).unwrap(),
            0xAFC0_5511,
            SimTick(120),
        )
    }

    #[test]
    fn couch_seats_are_owned_by_one_peer_and_authority_is_ordered_first() {
        let host = peer(20);
        let remote = peer(10);
        let mut roster = OnlineRoster::default();
        // Deliberately insert remote first and host last.
        roster
            .upsert(
                OnlineRosterMember::new(remote, user(110), 1, true, &[selection(2, 1)]).unwrap(),
            )
            .unwrap();
        roster
            .upsert(
                OnlineRosterMember::new(
                    host,
                    user(120),
                    1,
                    true,
                    &[selection(0, 0), selection(1, 0)],
                )
                .unwrap(),
            )
            .unwrap();

        let config = roster
            .build_headless_config(options(host), SimTick::ZERO)
            .unwrap();
        let ownership = config.manifest.ownership.as_slice();
        assert_eq!(ownership.len(), 3);
        assert_eq!(ownership[0].owner, SeatOwner::Peer(host));
        assert_eq!(ownership[1].owner, SeatOwner::Peer(host));
        assert_eq!(ownership[2].owner, SeatOwner::Peer(remote));
        assert_eq!(config.local_setup.slots[0].character as u8, 0);
        assert_eq!(config.local_setup.slots[1].character as u8, 1);
        assert_eq!(config.local_setup.slots[2].character as u8, 2);
    }

    #[test]
    fn arrival_order_does_not_change_manifest_identity() {
        let host = peer(8);
        let remote = peer(9);
        let declarations = [
            OnlineRosterMember::new(host, user(18), 1, true, &[selection(0, 0)]).unwrap(),
            OnlineRosterMember::new(remote, user(19), 1, true, &[selection(1, 1)]).unwrap(),
        ];
        let mut forward = OnlineRoster::default();
        let mut reverse = OnlineRoster::default();
        for declaration in declarations {
            forward.upsert(declaration).unwrap();
        }
        for declaration in declarations.into_iter().rev() {
            reverse.upsert(declaration).unwrap();
        }
        let left = forward
            .build_headless_config(options(host), SimTick::ZERO)
            .unwrap();
        let right = reverse
            .build_headless_config(options(host), SimTick::ZERO)
            .unwrap();
        assert_eq!(left.manifest, right.manifest);
    }

    #[test]
    fn loadout_change_requires_new_revision_and_ready_roster() {
        let host = peer(1);
        let mut roster = OnlineRoster::default();
        roster
            .upsert(OnlineRosterMember::new(host, user(11), 2, false, &[selection(0, 0)]).unwrap())
            .unwrap();
        assert!(matches!(
            roster.build_headless_config(options(host), SimTick::ZERO),
            Err(OnlineRosterError::NotReady(id)) if id == host
        ));
        assert_eq!(
            roster.upsert(
                OnlineRosterMember::new(host, user(11), 2, true, &[selection(1, 0)]).unwrap(),
            ),
            Err(OnlineRosterError::StaleRevision)
        );
        roster
            .upsert(OnlineRosterMember::new(host, user(11), 3, true, &[selection(1, 0)]).unwrap())
            .unwrap();
        assert!(
            roster
                .build_headless_config(options(host), SimTick::ZERO)
                .is_ok()
        );
    }

    #[test]
    fn duplicate_identity_and_total_seat_overflow_fail_closed() {
        let mut roster = OnlineRoster::default();
        roster
            .upsert(
                OnlineRosterMember::new(peer(1), user(11), 1, true, &[selection(0, 0); 3]).unwrap(),
            )
            .unwrap();
        assert!(matches!(
            roster.upsert(
                OnlineRosterMember::new(peer(2), user(11), 1, true, &[selection(1, 1)]).unwrap()
            ),
            Err(OnlineRosterError::DuplicateAuthenticatedUser(id)) if id == user(11)
        ));
        assert_eq!(
            roster.upsert(
                OnlineRosterMember::new(peer(2), user(12), 1, true, &[selection(1, 1); 2],)
                    .unwrap(),
            ),
            Err(OnlineRosterError::SeatCapacity)
        );
    }

    #[test]
    fn trusted_listen_manifest_is_rejected_by_shared_protocol_policy() {
        let host = peer(1);
        let mut roster = OnlineRoster::default();
        roster
            .upsert(OnlineRosterMember::new(host, user(11), 1, true, &[selection(0, 0)]).unwrap())
            .unwrap();
        let mut invalid = options(host);
        invalid.trusted_results = true;
        assert!(matches!(
            roster.build_headless_config(invalid, SimTick::ZERO),
            Err(OnlineRosterError::MatchConfig(
                crate::match_config::MatchConfigError::Protocol(
                    ProtocolValidationError::UntrustedAuthorityForTrustedResult
                )
            ))
        ));
    }

    #[test]
    fn first_release_policy_accepts_only_untrusted_listen_manifests() {
        let host = peer(1);
        let casual = options(host);
        assert!(FirstReleaseOnlinePolicy::accepts_options(&casual));
        assert!(!FirstReleaseOnlinePolicy::PUBLIC_DISCOVERY_ENABLED);
        assert!(!FirstReleaseOnlinePolicy::HOSTED_DEDICATED_ENABLED);
        assert!(!FirstReleaseOnlinePolicy::RANKED_PLAY_ENABLED);
        assert!(!FirstReleaseOnlinePolicy::TRUSTED_RESULTS_ENABLED);

        let mut trusted = casual;
        trusted.trusted_results = true;
        assert!(!FirstReleaseOnlinePolicy::accepts_options(&trusted));

        let mut dedicated = casual;
        dedicated.authority = AuthorityKind::Dedicated;
        dedicated.authority_peer = None;
        assert!(!FirstReleaseOnlinePolicy::accepts_options(&dedicated));

        let mut roster = OnlineRoster::default();
        roster
            .upsert(OnlineRosterMember::new(host, user(11), 1, true, &[selection(0, 0)]).unwrap())
            .unwrap();
        let config = roster.build_headless_config(casual, SimTick::ZERO).unwrap();
        assert!(FirstReleaseOnlinePolicy::accepts_manifest(&config.manifest));
    }

    #[test]
    fn member_declaration_codec_is_canonical_bounded_and_round_trips_four_seats() {
        let seats = [
            selection(0, 0),
            selection(1, 1),
            selection(2, 0),
            selection(3, 1),
        ];
        let member = OnlineRosterMember::new(peer(7), user(17), 0x1234, true, &seats).unwrap();
        let encoded = encode_member_declaration(&member);
        assert!(encoded.len() <= MAX_ONLINE_MEMBER_DECLARATION_BYTES);
        assert!(
            encoded
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        );
        assert_eq!(
            decode_member_declaration(peer(7), user(17), true, &encoded).unwrap(),
            member
        );

        let mut uppercase = encoded;
        uppercase.replace_range(0..1, "A");
        assert_eq!(
            decode_member_declaration(peer(7), user(17), true, &uppercase),
            Err(OnlineRosterError::InvalidDeclaration)
        );
    }

    #[test]
    fn member_declaration_codec_rejects_noncanonical_lengths_versions_and_ids() {
        assert_eq!(
            decode_member_declaration(peer(1), user(11), false, "01"),
            Err(OnlineRosterError::InvalidDeclaration)
        );
        // Version 2, revision 1, one seat, followed by one otherwise valid seat.
        assert_eq!(
            decode_member_declaration(peer(1), user(11), false, "0201000100000000000000"),
            Err(OnlineRosterError::InvalidDeclaration)
        );
        assert_eq!(
            decode_member_declaration(peer(1), user(11), false, &"0".repeat(66)),
            Err(OnlineRosterError::InvalidDeclaration)
        );
    }
}
