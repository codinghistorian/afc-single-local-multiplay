//! Production construction of immutable match manifests.
//!
//! A manifest is the bridge between lobby/setup state and the deterministic
//! simulation.  This module owns the stable definition-ID mappings and hashes
//! every agreed field explicitly; callers must never hash Rust memory layouts.

use std::error::Error;
use std::fmt;
use std::sync::OnceLock;

use crate::arena_defs::arena_definitions;
use crate::characters::{CHARACTER_KINDS, CharacterKind};
use crate::components::{LocalInputAssignment, ParticipantKind};
use crate::determinism::{CanonicalHash64, FighterId, SimTick};
use crate::equipment::EquipmentKind;
use crate::game_state::{LocalSetup, RULE_PRESETS, TeamId};
use crate::headless::{HeadlessMatchConfig, snapshot_contract_for_manifest};
use crate::network_protocol::{
    AuthorityKind, BuildId, CompatibilityId, DefinitionId, FighterSlotConfig, GameplayContentHash,
    MAX_FIGHTERS, MAX_NORMAL_ROLLBACK_TICKS, ManifestHash, MatchId, MatchManifest, PeerId,
    ProtocolValidationError, ProtocolVersion, ReplayFormatVersion, SIMULATION_HZ, SeatAssignment,
    SeatId, SeatOwner, SeatOwnership, SimulationVersion, TeamId as ProtocolTeamId,
};
use crate::replay::REPLAY_SCHEMA_VERSION;
use crate::styles::FighterStyleKind;

/// Bump these only when the corresponding migration policy is intentionally
/// changed. Source/content digests below still prevent accidentally mixing two
/// development binaries that use the same numeric schema versions.
pub const CURRENT_PROTOCOL_VERSION: u16 = 1;
pub const CURRENT_SIMULATION_VERSION: u16 = 5;
pub const CURRENT_RNG_SCHEME_VERSION: u16 = 1;

pub const DEFAULT_INPUT_DELAY_TICKS: u8 = 2;
pub const DEFAULT_ROLLBACK_LIMIT_TICKS: u8 = MAX_NORMAL_ROLLBACK_TICKS;
pub const DEFAULT_SNAPSHOT_HISTORY_TICKS: u8 = 64;

/// Immutable session values supplied by the lobby/authority boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MatchBuildOptions {
    pub match_id: MatchId,
    pub authority: AuthorityKind,
    pub trusted_results: bool,
    /// Human ownership by fighter slot. Bot and closed slots must be `None`.
    pub human_owners: [Option<PeerId>; MAX_FIGHTERS],
    pub agreed_start_tick: SimTick,
    pub input_delay_ticks: u8,
    pub rollback_limit_ticks: u8,
    pub snapshot_history_ticks: u8,
}

impl MatchBuildOptions {
    pub fn single_peer(
        match_id: MatchId,
        authority: AuthorityKind,
        trusted_results: bool,
        peer: PeerId,
        setup: &LocalSetup,
        agreed_start_tick: SimTick,
    ) -> Self {
        let mut human_owners = [None; MAX_FIGHTERS];
        for (index, slot) in setup.slots.iter().enumerate() {
            if slot.participant == ParticipantKind::Human {
                human_owners[index] = Some(peer);
            }
        }
        Self {
            match_id,
            authority,
            trusted_results,
            human_owners,
            agreed_start_tick,
            input_delay_ticks: DEFAULT_INPUT_DELAY_TICKS,
            rollback_limit_ticks: DEFAULT_ROLLBACK_LIMIT_TICKS,
            snapshot_history_ticks: DEFAULT_SNAPSHOT_HISTORY_TICKS,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MatchConfigError {
    Protocol(ProtocolValidationError),
    InvalidArena(usize),
    InvalidRules(usize),
    NoActiveFighters,
    MissingHumanOwner(FighterId),
    OwnerForNonHuman(FighterId),
    ManifestHashMismatch {
        received: ManifestHash,
        expected: ManifestHash,
    },
    UnknownDefinition {
        field: &'static str,
        value: u16,
        fighter: Option<FighterId>,
    },
}

impl fmt::Display for MatchConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid deterministic match configuration: {self:?}"
        )
    }
}

impl Error for MatchConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Protocol(error) => Some(error),
            _ => None,
        }
    }
}

impl From<ProtocolValidationError> for MatchConfigError {
    fn from(error: ProtocolValidationError) -> Self {
        Self::Protocol(error)
    }
}

/// Current exact-match compatibility identity.
///
/// Release automation should set `AFC_BUILD_ID` to an immutable build/commit
/// identity. Developer builds additionally digest protocol and simulation source
/// contracts, so independently compiled incompatible trees fail agreement.
pub fn current_compatibility() -> CompatibilityId {
    static CURRENT: OnceLock<CompatibilityId> = OnceLock::new();
    *CURRENT.get_or_init(|| CompatibilityId {
        protocol: ProtocolVersion::new(CURRENT_PROTOCOL_VERSION)
            .expect("the current protocol version is non-zero"),
        simulation: SimulationVersion::new(CURRENT_SIMULATION_VERSION)
            .expect("the current simulation version is non-zero"),
        replay: ReplayFormatVersion::new(REPLAY_SCHEMA_VERSION)
            .expect("the current replay version is non-zero"),
        build: current_build_id(),
        gameplay_content: current_gameplay_content_hash(),
    })
}

/// Builds the manifest and the exact headless bootstrap contract from local setup.
/// The same value can bootstrap offline, listen, and dedicated authorities.
pub fn build_headless_match_config(
    setup: &LocalSetup,
    options: MatchBuildOptions,
) -> Result<HeadlessMatchConfig, MatchConfigError> {
    if setup.arena_index >= arena_definitions().len() {
        return Err(MatchConfigError::InvalidArena(setup.arena_index));
    }
    if setup.rule_index >= RULE_PRESETS.len() {
        return Err(MatchConfigError::InvalidRules(setup.rule_index));
    }

    let mut slots = [FighterSlotConfig::default(); MAX_FIGHTERS];
    let mut assignments = [SeatAssignment::default(); MAX_FIGHTERS];
    let mut assignment_count = 0;

    for fighter in FighterId::ALL {
        let index = fighter.index();
        let slot = setup.slots[index];
        let owner = match slot.participant {
            ParticipantKind::Human => SeatOwner::Peer(
                options.human_owners[index].ok_or(MatchConfigError::MissingHumanOwner(fighter))?,
            ),
            ParticipantKind::Bot => {
                if options.human_owners[index].is_some() {
                    return Err(MatchConfigError::OwnerForNonHuman(fighter));
                }
                SeatOwner::AuthorityBot
            }
            ParticipantKind::Closed => {
                if options.human_owners[index].is_some() {
                    return Err(MatchConfigError::OwnerForNonHuman(fighter));
                }
                continue;
            }
        };

        let seat = SeatId::new(fighter.get())?;
        assignments[assignment_count] = SeatAssignment {
            seat,
            fighter,
            owner,
        };
        assignment_count += 1;
        slots[index] = FighterSlotConfig {
            occupied: true,
            fighter,
            team: ProtocolTeamId::new(team_definition_id(slot.team))?,
            character: DefinitionId::new(character_definition_id(slot.character))?,
            style: DefinitionId::new(style_definition_id(slot.style))?,
            equipment: DefinitionId::new(equipment_definition_id(slot.equipment))?,
        };
    }

    if assignment_count == 0 {
        return Err(MatchConfigError::NoActiveFighters);
    }
    let ownership = SeatOwnership::from_assignments(&assignments[..assignment_count])?;
    let mut manifest = MatchManifest {
        compatibility: current_compatibility(),
        manifest_hash: ManifestHash(0),
        match_id: options.match_id,
        authority: options.authority,
        trusted_results: options.trusted_results,
        arena: DefinitionId::new(setup.arena_index as u16)?,
        rules: DefinitionId::new(setup.rule_index as u16)?,
        slots,
        ownership,
        master_gameplay_seed: setup.replay_seed,
        rng_scheme_version: CURRENT_RNG_SCHEME_VERSION,
        tick_rate_hz: SIMULATION_HZ,
        input_delay_ticks: options.input_delay_ticks,
        rollback_limit_ticks: options.rollback_limit_ticks,
        snapshot_history_ticks: options.snapshot_history_ticks,
        agreed_start_tick: options.agreed_start_tick,
    };
    manifest.manifest_hash = canonical_manifest_hash(&manifest);
    manifest.validate_for_start(SimTick::ZERO)?;

    Ok(HeadlessMatchConfig {
        snapshot_contract: snapshot_contract_for_manifest(&manifest),
        manifest,
        local_setup: setup.clone(),
    })
}

/// Reconstructs server bootstrap state from a manifest received at the platform
/// boundary. This is the dedicated-authority path: it validates exact build/content
/// compatibility and the manifest digest before any world is constructed.
pub fn headless_config_from_manifest(
    manifest: MatchManifest,
) -> Result<HeadlessMatchConfig, MatchConfigError> {
    headless_config_from_manifest_at(manifest, SimTick::ZERO)
}

/// Same as [`headless_config_from_manifest`], additionally rejecting a start
/// boundary that has already elapsed on the session clock.
pub fn headless_config_from_manifest_at(
    manifest: MatchManifest,
    current_tick: SimTick,
) -> Result<HeadlessMatchConfig, MatchConfigError> {
    manifest.validate_for_start(current_tick)?;
    manifest
        .compatibility
        .validate_against(&current_compatibility())?;
    let expected_hash = canonical_manifest_hash(&manifest);
    if manifest.manifest_hash != expected_hash {
        return Err(MatchConfigError::ManifestHashMismatch {
            received: manifest.manifest_hash,
            expected: expected_hash,
        });
    }

    let arena_index = usize::from(manifest.arena.get());
    if arena_index >= arena_definitions().len() {
        return Err(MatchConfigError::InvalidArena(arena_index));
    }
    let rule_index = usize::from(manifest.rules.get());
    if rule_index >= RULE_PRESETS.len() {
        return Err(MatchConfigError::InvalidRules(rule_index));
    }
    if manifest.ownership.is_empty() {
        return Err(MatchConfigError::NoActiveFighters);
    }

    let mut setup = LocalSetup::default();
    setup.arena_index = arena_index;
    setup.rule_index = rule_index;
    setup.replay_seed = manifest.master_gameplay_seed;
    for fighter in FighterId::ALL {
        let wire = manifest.slots[fighter.index()];
        let slot = &mut setup.slots[fighter.index()];
        slot.input = LocalInputAssignment::Unassigned;
        if !wire.occupied {
            slot.participant = ParticipantKind::Closed;
            continue;
        }
        let owner = manifest
            .ownership
            .assignment_for_fighter(fighter)
            .expect("validated occupied manifest slot has an owner")
            .owner;
        slot.participant = match owner {
            SeatOwner::Peer(_) => ParticipantKind::Human,
            SeatOwner::AuthorityBot => ParticipantKind::Bot,
        };
        slot.character = *CHARACTER_KINDS
            .get(usize::from(wire.character.get()))
            .ok_or(MatchConfigError::UnknownDefinition {
                field: "character",
                value: wire.character.get(),
                fighter: Some(fighter),
            })?;
        slot.style = style_from_definition_id(wire.style.get()).ok_or(
            MatchConfigError::UnknownDefinition {
                field: "style",
                value: wire.style.get(),
                fighter: Some(fighter),
            },
        )?;
        slot.equipment = equipment_from_definition_id(wire.equipment.get()).ok_or(
            MatchConfigError::UnknownDefinition {
                field: "equipment",
                value: wire.equipment.get(),
                fighter: Some(fighter),
            },
        )?;
        slot.team = team_from_definition_id(wire.team.get()).ok_or(
            MatchConfigError::UnknownDefinition {
                field: "team",
                value: u16::from(wire.team.get()),
                fighter: Some(fighter),
            },
        )?;
    }

    let config = HeadlessMatchConfig {
        snapshot_contract: snapshot_contract_for_manifest(&manifest),
        manifest,
        local_setup: setup,
    };
    // Keep the public conversion fail-closed if the bootstrap contract gains a
    // field that this inverse mapping has not yet learned.
    config
        .validate()
        .map_err(|_| MatchConfigError::Protocol(ProtocolValidationError::InvalidManifest))?;
    Ok(config)
}

/// Recomputes the stable identity of all agreed manifest fields except the hash.
pub fn canonical_manifest_hash(manifest: &MatchManifest) -> ManifestHash {
    let mut hash = CanonicalHash64::new();
    hash.write_str("afc-match-manifest-v1")
        .write_u16(manifest.compatibility.protocol.get())
        .write_u16(manifest.compatibility.simulation.get())
        .write_u16(manifest.compatibility.replay.get())
        .write_bytes(manifest.compatibility.build.as_bytes())
        .write_bytes(manifest.compatibility.gameplay_content.as_bytes())
        .write_bytes(manifest.match_id.as_bytes())
        .write_u8(authority_code(manifest.authority))
        .write_bool(manifest.trusted_results)
        .write_u16(manifest.arena.get())
        .write_u16(manifest.rules.get());

    for slot in manifest.slots {
        hash.write_bool(slot.occupied)
            .write_u8(slot.fighter.get())
            .write_u8(slot.team.get())
            .write_u16(slot.character.get())
            .write_u16(slot.style.get())
            .write_u16(slot.equipment.get());
    }
    hash.write_u8(manifest.ownership.len() as u8);
    for assignment in manifest.ownership.as_slice() {
        hash.write_u8(assignment.seat.get())
            .write_u8(assignment.fighter.get());
        match assignment.owner {
            SeatOwner::Peer(peer) => {
                hash.write_u8(0).write_u64(peer.get());
            }
            SeatOwner::AuthorityBot => {
                hash.write_u8(1);
            }
        }
    }
    hash.write_u64(manifest.master_gameplay_seed)
        .write_u16(manifest.rng_scheme_version)
        .write_u16(manifest.tick_rate_hz)
        .write_u8(manifest.input_delay_ticks)
        .write_u8(manifest.rollback_limit_ticks)
        .write_u8(manifest.snapshot_history_ticks)
        .write_u64(manifest.agreed_start_tick.get());

    // Zero is reserved as an obvious "not computed" sentinel by construction.
    let value = hash.finish();
    ManifestHash(if value == 0 { u64::MAX } else { value })
}

fn current_build_id() -> BuildId {
    let digest = decode_compiled_hex::<16>(env!("AFC_COMPILED_BUILD_ID"));
    BuildId::new(digest).expect("a labeled digest cannot be the all-zero build ID")
}

fn current_gameplay_content_hash() -> GameplayContentHash {
    let digest = decode_compiled_hex::<32>(env!("AFC_COMPILED_GAMEPLAY_CONTENT_HASH"));
    GameplayContentHash::new(digest)
        .expect("a labeled digest cannot be the all-zero gameplay content ID")
}

fn decode_compiled_hex<const N: usize>(encoded: &str) -> [u8; N] {
    assert_eq!(encoded.len(), N * 2, "compiled digest width is invalid");
    let mut output = [0_u8; N];
    for (index, byte) in output.iter_mut().enumerate() {
        let offset = index * 2;
        *byte = (hex_nibble(encoded.as_bytes()[offset]) << 4)
            | hex_nibble(encoded.as_bytes()[offset + 1]);
    }
    output
}

const fn hex_nibble(value: u8) -> u8 {
    match value {
        b'0'..=b'9' => value - b'0',
        b'a'..=b'f' => value - b'a' + 10,
        b'A'..=b'F' => value - b'A' + 10,
        _ => panic!("compiled digest contains a non-hex character"),
    }
}

fn character_definition_id(character: CharacterKind) -> u16 {
    CHARACTER_KINDS
        .iter()
        .position(|candidate| *candidate == character)
        .expect("every CharacterKind is cataloged") as u16
}

fn style_definition_id(style: FighterStyleKind) -> u16 {
    match style {
        FighterStyleKind::Anchor => 0,
        FighterStyleKind::Vector => 1,
        FighterStyleKind::Catalyst => 2,
    }
}

fn equipment_definition_id(equipment: EquipmentKind) -> u16 {
    match equipment {
        EquipmentKind::DashCoil => 0,
        EquipmentKind::AerialSpur => 1,
        EquipmentKind::CounterCell => 2,
        EquipmentKind::HeavySeal => 3,
    }
}

const fn style_from_definition_id(value: u16) -> Option<FighterStyleKind> {
    match value {
        0 => Some(FighterStyleKind::Anchor),
        1 => Some(FighterStyleKind::Vector),
        2 => Some(FighterStyleKind::Catalyst),
        _ => None,
    }
}

const fn equipment_from_definition_id(value: u16) -> Option<EquipmentKind> {
    match value {
        0 => Some(EquipmentKind::DashCoil),
        1 => Some(EquipmentKind::AerialSpur),
        2 => Some(EquipmentKind::CounterCell),
        3 => Some(EquipmentKind::HeavySeal),
        _ => None,
    }
}

const fn team_from_definition_id(value: u8) -> Option<TeamId> {
    match value {
        0 => Some(TeamId::Red),
        1 => Some(TeamId::Blue),
        _ => None,
    }
}

const fn team_definition_id(team: TeamId) -> u8 {
    match team {
        TeamId::Red => 0,
        TeamId::Blue => 1,
    }
}

const fn authority_code(authority: AuthorityKind) -> u8 {
    match authority {
        AuthorityKind::Offline => 0,
        AuthorityKind::Listen => 1,
        AuthorityKind::Dedicated => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn match_id() -> MatchId {
        MatchId::new(*b"manifest-build-1").unwrap()
    }

    fn peer(value: u64) -> PeerId {
        PeerId::new(value).unwrap()
    }

    #[test]
    fn single_peer_setup_builds_a_valid_headless_contract() {
        let setup = LocalSetup::default();
        let options = MatchBuildOptions::single_peer(
            match_id(),
            AuthorityKind::Listen,
            false,
            peer(7),
            &setup,
            SimTick(120),
        );
        let config = build_headless_match_config(&setup, options).unwrap();

        config.validate().unwrap();
        assert_ne!(config.manifest.manifest_hash, ManifestHash(0));
        assert_eq!(
            canonical_manifest_hash(&config.manifest),
            config.manifest.manifest_hash
        );
        assert_eq!(config.manifest.ownership.len(), 2);
        assert_eq!(
            config.manifest.ownership.as_slice()[0].owner,
            SeatOwner::Peer(peer(7))
        );
        assert_eq!(
            config.manifest.ownership.as_slice()[1].owner,
            SeatOwner::AuthorityBot
        );
        assert_eq!(config.manifest.slots[2], FighterSlotConfig::default());
    }

    #[test]
    fn multiple_peers_are_preserved_in_canonical_fighter_order() {
        let mut setup = LocalSetup::default();
        setup.slots[1].participant = ParticipantKind::Human;
        let options = MatchBuildOptions {
            human_owners: [Some(peer(10)), Some(peer(20)), None, None],
            ..MatchBuildOptions::single_peer(
                match_id(),
                AuthorityKind::Dedicated,
                true,
                peer(10),
                &setup,
                SimTick(90),
            )
        };
        let config = build_headless_match_config(&setup, options).unwrap();

        assert_eq!(
            config.manifest.ownership.as_slice()[0].owner,
            SeatOwner::Peer(peer(10))
        );
        assert_eq!(
            config.manifest.ownership.as_slice()[1].owner,
            SeatOwner::Peer(peer(20))
        );
        assert!(config.manifest.trusted_results);
    }

    #[test]
    fn ownership_must_match_participant_kind_exactly() {
        let setup = LocalSetup::default();
        let missing = MatchBuildOptions {
            human_owners: [None; MAX_FIGHTERS],
            ..MatchBuildOptions::single_peer(
                match_id(),
                AuthorityKind::Offline,
                false,
                peer(1),
                &setup,
                SimTick(1),
            )
        };
        assert!(matches!(
            build_headless_match_config(&setup, missing),
            Err(MatchConfigError::MissingHumanOwner(FighterId::ZERO))
        ));

        let mut extra = MatchBuildOptions::single_peer(
            match_id(),
            AuthorityKind::Offline,
            false,
            peer(1),
            &setup,
            SimTick(1),
        );
        extra.human_owners[1] = Some(peer(2));
        assert!(matches!(
            build_headless_match_config(&setup, extra),
            Err(MatchConfigError::OwnerForNonHuman(fighter))
                if fighter == FighterId::new(1).unwrap()
        ));
    }

    #[test]
    fn every_agreed_field_contributes_to_manifest_hash() {
        let setup = LocalSetup::default();
        let options = MatchBuildOptions::single_peer(
            match_id(),
            AuthorityKind::Listen,
            false,
            peer(4),
            &setup,
            SimTick(100),
        );
        let config = build_headless_match_config(&setup, options).unwrap();
        let expected = config.manifest.manifest_hash;

        let mut changed = config.manifest;
        changed.master_gameplay_seed ^= 1;
        assert_ne!(canonical_manifest_hash(&changed), expected);
        changed = config.manifest;
        changed.input_delay_ticks += 1;
        assert_ne!(canonical_manifest_hash(&changed), expected);
        changed = config.manifest;
        changed.manifest_hash = ManifestHash(123);
        assert_eq!(canonical_manifest_hash(&changed), expected);
    }

    #[test]
    fn current_compatibility_is_valid_and_repeatable() {
        let first = current_compatibility();
        let second = current_compatibility();
        first.validate().unwrap();
        assert_eq!(first, second);
        assert_eq!(first.simulation.get(), 5);
        assert_eq!(first.replay.get(), REPLAY_SCHEMA_VERSION);
    }

    #[test]
    fn v4_client_is_rejected_by_the_v5_lobby_handshake() {
        let expected = current_compatibility();
        let request = crate::network_protocol::LobbyJoinRequest {
            compatibility: CompatibilityId {
                simulation: SimulationVersion::new(4).unwrap(),
                ..expected
            },
            requested_local_seats: 1,
            reconnect: None,
        };

        assert_eq!(
            request.validate(&expected),
            Err(ProtocolValidationError::SimulationVersionMismatch)
        );
    }

    #[test]
    fn authority_reconstructs_the_exact_setup_from_a_valid_manifest() {
        let mut setup = LocalSetup::default();
        setup.slots[1].participant = ParticipantKind::Human;
        setup.slots[1].character = CharacterKind::Penguin;
        setup.slots[1].style = FighterStyleKind::Catalyst;
        setup.slots[1].equipment = EquipmentKind::HeavySeal;
        setup.slots[1].team = TeamId::Red;
        let options = MatchBuildOptions {
            human_owners: [Some(peer(11)), Some(peer(22)), None, None],
            ..MatchBuildOptions::single_peer(
                match_id(),
                AuthorityKind::Dedicated,
                true,
                peer(11),
                &setup,
                SimTick(120),
            )
        };
        let original = build_headless_match_config(&setup, options).unwrap();
        let reconstructed = headless_config_from_manifest(original.manifest).unwrap();

        reconstructed.validate().unwrap();
        assert_eq!(reconstructed.local_setup.rule_index, setup.rule_index);
        assert_eq!(reconstructed.local_setup.arena_index, setup.arena_index);
        assert_eq!(reconstructed.local_setup.replay_seed, setup.replay_seed);
        for index in 0..MAX_FIGHTERS {
            let mut expected = setup.slots[index];
            // Raw device ownership is deliberately absent from a wire manifest.
            expected.input = LocalInputAssignment::Unassigned;
            assert_eq!(reconstructed.local_setup.slots[index], expected);
        }
    }

    #[test]
    fn authority_rejects_tampered_or_unknown_manifest_values_before_bootstrap() {
        let setup = LocalSetup::default();
        let options = MatchBuildOptions::single_peer(
            match_id(),
            AuthorityKind::Listen,
            false,
            peer(7),
            &setup,
            SimTick(120),
        );
        let config = build_headless_match_config(&setup, options).unwrap();

        let mut tampered = config.manifest;
        tampered.master_gameplay_seed ^= 1;
        assert!(matches!(
            headless_config_from_manifest(tampered),
            Err(MatchConfigError::ManifestHashMismatch { .. })
        ));

        let mut unknown = config.manifest;
        unknown.slots[0].style = DefinitionId::new(77).unwrap();
        unknown.manifest_hash = canonical_manifest_hash(&unknown);
        assert!(matches!(
            headless_config_from_manifest(unknown),
            Err(MatchConfigError::UnknownDefinition {
                field: "style",
                value: 77,
                fighter: Some(FighterId::ZERO)
            })
        ));

        assert!(matches!(
            headless_config_from_manifest_at(config.manifest, SimTick(120)),
            Err(MatchConfigError::Protocol(
                ProtocolValidationError::InvalidStartTick
            ))
        ));
    }
}
