//! Render-only online match presentation derived from confirmed authority data.
//!
//! Nothing in this module is canonical simulation state.  The types here are
//! deliberately typed (rather than preformatted strings) so HUD, native UI,
//! audio, and future platform surfaces all present the same confirmed result.

use bevy::prelude::*;

use crate::confirmed_progression::{ConfirmedProgressionRecord, ConfirmedResultKey};
use crate::determinism::{FIGHTER_CAPACITY, FighterId};
use crate::network_protocol::{DefinitionId, MatchManifest, PeerId, SeatOwner, TeamId};
use crate::snapshot::{FighterMatchStatsSnapshot, MatchResultSnapshot};

/// Marks match-scoped render entities which must not survive a projection
/// target release or the preparation of a new match.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MatchPresentationTransient;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresentedMatchOutcome {
    FighterWinner(FighterId),
    TeamWinner(TeamId),
    Draw,
    Aborted(PresentedAbortReason),
}

/// Player-facing no-contest cause. Authority codes remain lossless without
/// making an untyped integer the presentation API.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresentedAbortReason {
    HostLost,
    SessionFailure,
    Authority(u16),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresentedLocalOutcome {
    Victory,
    Defeat,
    Draw,
    Mixed,
    NoContest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PresentedFighterStats {
    pub fighter: FighterId,
    pub team: TeamId,
    pub character: DefinitionId,
    pub locally_owned: bool,
    pub stats: FighterMatchStatsSnapshot,
}

#[derive(Resource, Clone, Debug, PartialEq, Eq)]
pub struct ConfirmedMatchPresentation {
    pub key: ConfirmedResultKey,
    pub final_tick: crate::simulation::SimTick,
    pub outcome: PresentedMatchOutcome,
    pub local_outcome: PresentedLocalOutcome,
    /// Fixed fighter-slot matrix. Unoccupied manifest slots remain `None`.
    pub fighters: [Option<PresentedFighterStats>; FIGHTER_CAPACITY as usize],
}

impl ConfirmedMatchPresentation {
    pub fn from_confirmed_record(
        manifest: &MatchManifest,
        local_peer: PeerId,
        record: &ConfirmedProgressionRecord,
    ) -> Self {
        let outcome = match record.result {
            MatchResultSnapshot::Pending => {
                unreachable!("confirmed progression rejects pending results")
            }
            MatchResultSnapshot::FighterWinner { fighter, .. } => {
                PresentedMatchOutcome::FighterWinner(fighter)
            }
            MatchResultSnapshot::TeamWinner { team, .. } => PresentedMatchOutcome::TeamWinner(
                TeamId::new(team).expect("confirmed snapshot team is manifest-valid"),
            ),
            MatchResultSnapshot::Draw { .. } => PresentedMatchOutcome::Draw,
            MatchResultSnapshot::Aborted { reason, .. } => {
                PresentedMatchOutcome::Aborted(PresentedAbortReason::Authority(reason))
            }
        };

        let fighters = std::array::from_fn(|index| {
            let slot = manifest.slots[index];
            slot.occupied.then(|| PresentedFighterStats {
                fighter: slot.fighter,
                team: slot.team,
                character: slot.character,
                locally_owned: manifest
                    .ownership
                    .assignment_for_fighter(slot.fighter)
                    .is_some_and(|assignment| assignment.owner == SeatOwner::Peer(local_peer)),
                stats: record.stats.fighter[index],
            })
        });

        Self {
            key: record.key,
            final_tick: record.final_tick,
            local_outcome: local_outcome(outcome, &fighters),
            outcome,
            fighters,
        }
    }
}

fn local_outcome(
    outcome: PresentedMatchOutcome,
    fighters: &[Option<PresentedFighterStats>; FIGHTER_CAPACITY as usize],
) -> PresentedLocalOutcome {
    match outcome {
        PresentedMatchOutcome::Draw => PresentedLocalOutcome::Draw,
        PresentedMatchOutcome::Aborted(_) => PresentedLocalOutcome::NoContest,
        PresentedMatchOutcome::FighterWinner(winner) => {
            classify_local_winners(fighters, |fighter| fighter.fighter == winner)
        }
        PresentedMatchOutcome::TeamWinner(winner) => {
            classify_local_winners(fighters, |fighter| fighter.team == winner)
        }
    }
}

fn classify_local_winners(
    fighters: &[Option<PresentedFighterStats>; FIGHTER_CAPACITY as usize],
    won: impl Fn(PresentedFighterStats) -> bool,
) -> PresentedLocalOutcome {
    let mut winners = 0_u8;
    let mut losers = 0_u8;
    for fighter in fighters.iter().flatten().copied() {
        if !fighter.locally_owned {
            continue;
        }
        if won(fighter) {
            winners = winners.saturating_add(1);
        } else {
            losers = losers.saturating_add(1);
        }
    }
    match (winners > 0, losers > 0) {
        (true, true) => PresentedLocalOutcome::Mixed,
        (true, false) => PresentedLocalOutcome::Victory,
        (false, true) => PresentedLocalOutcome::Defeat,
        // Bootstrap validation requires a local seat, but keep this render
        // boundary total if a diagnostic world constructs an unusual manifest.
        (false, false) => PresentedLocalOutcome::NoContest,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PresentationPhase {
    #[default]
    Offline,
    Menu,
    Lobby,
    Countdown,
    Fighting,
    Reconnecting,
    ConfirmingResult,
    Results,
    LeaveConfirmation,
    Error,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OnlinePanelMode {
    #[default]
    Hidden,
    Full,
    CountdownStrip,
    FightStrip,
    ReconnectStrip,
    ConfirmingStrip,
    Results,
    LeaveConfirmation,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PresentationMusicTrack {
    #[default]
    None,
    Menu,
    Arena(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresentationResultSfx {
    Victory,
    Defeat,
}

/// One shared render policy, derived once per frame before all presentation
/// consumers run.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MatchPresentationPolicy {
    pub phase: PresentationPhase,
    pub panel: OnlinePanelMode,
    pub gameplay_hud_visible: bool,
    pub music: PresentationMusicTrack,
    pub result_sfx: Option<(ConfirmedResultKey, PresentationResultSfx)>,
}

const PRESENTED_RESULT_SFX_HISTORY_CAPACITY: usize = 64;

#[derive(Resource, Debug)]
pub struct PresentedResultSfxHistory {
    keys: [Option<ConfirmedResultKey>; PRESENTED_RESULT_SFX_HISTORY_CAPACITY],
    next: usize,
}

impl Default for PresentedResultSfxHistory {
    fn default() -> Self {
        Self {
            keys: [None; PRESENTED_RESULT_SFX_HISTORY_CAPACITY],
            next: 0,
        }
    }
}

impl PresentedResultSfxHistory {
    pub fn mark_if_new(&mut self, key: ConfirmedResultKey) -> bool {
        if self.keys.contains(&Some(key)) {
            return false;
        }
        self.keys[self.next] = Some(key);
        self.next = (self.next + 1) % PRESENTED_RESULT_SFX_HISTORY_CAPACITY;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::confirmed_progression::{ConfirmedProgressionRecord, ProgressionTrust};
    use crate::network_protocol::{
        AuthorityKind, FighterSlotConfig, ManifestHash, MatchId, SeatAssignment, SeatId,
        SeatOwnership, StateHash,
    };
    use crate::snapshot::MatchStatsSnapshot;

    fn manifest(local_peer: PeerId) -> MatchManifest {
        let assignments = [
            SeatAssignment {
                seat: SeatId::new(0).unwrap(),
                fighter: FighterId::new(0).unwrap(),
                owner: SeatOwner::Peer(local_peer),
            },
            SeatAssignment {
                seat: SeatId::new(1).unwrap(),
                fighter: FighterId::new(1).unwrap(),
                owner: SeatOwner::Peer(local_peer),
            },
        ];
        let mut slots = [FighterSlotConfig::default(); 4];
        for index in 0..2 {
            slots[index] = FighterSlotConfig {
                occupied: true,
                fighter: FighterId::new(index as u8).unwrap(),
                team: TeamId::new(index as u8).unwrap(),
                character: DefinitionId::new(index as u16).unwrap(),
                style: DefinitionId::new(0).unwrap(),
                equipment: DefinitionId::new(0).unwrap(),
            };
        }
        MatchManifest {
            compatibility: crate::match_config::current_compatibility(),
            manifest_hash: ManifestHash(1),
            match_id: MatchId::new([1; 16]).unwrap(),
            authority: AuthorityKind::Listen,
            trusted_results: false,
            arena: DefinitionId::new(0).unwrap(),
            rules: DefinitionId::new(0).unwrap(),
            slots,
            ownership: SeatOwnership::from_assignments(&assignments).unwrap(),
            master_gameplay_seed: 1,
            rng_scheme_version: 1,
            tick_rate_hz: 60,
            input_delay_ticks: 2,
            rollback_limit_ticks: 12,
            snapshot_history_ticks: 32,
            agreed_start_tick: crate::simulation::SimTick(120),
        }
    }

    #[test]
    fn couch_result_classifies_split_winners_as_mixed() {
        let peer = PeerId::new(7).unwrap();
        let manifest = manifest(peer);
        let record = ConfirmedProgressionRecord {
            key: ConfirmedResultKey {
                match_id: manifest.match_id,
                result_id: 9,
            },
            final_tick: crate::simulation::SimTick(30),
            final_hash: StateHash(5),
            result: MatchResultSnapshot::FighterWinner {
                fighter: FighterId::new(0).unwrap(),
                decided_tick: crate::simulation::SimTick(30),
            },
            stats: MatchStatsSnapshot::default(),
            trust: ProgressionTrust::UntrustedCasual,
        };
        let result = ConfirmedMatchPresentation::from_confirmed_record(&manifest, peer, &record);
        assert_eq!(result.local_outcome, PresentedLocalOutcome::Mixed);
    }

    fn presented_fighter(
        fighter: u8,
        team: u8,
        locally_owned: bool,
    ) -> Option<PresentedFighterStats> {
        Some(PresentedFighterStats {
            fighter: FighterId::new(fighter).unwrap(),
            team: TeamId::new(team).unwrap(),
            character: DefinitionId::new(fighter.into()).unwrap(),
            locally_owned,
            stats: FighterMatchStatsSnapshot::default(),
        })
    }

    #[test]
    fn local_outcome_matrix_is_total_for_couch_and_no_local_seat() {
        let winner = FighterId::new(0).unwrap();
        let one_local_winner = [
            presented_fighter(0, 0, true),
            presented_fighter(1, 1, false),
            None,
            None,
        ];
        assert_eq!(
            local_outcome(
                PresentedMatchOutcome::FighterWinner(winner),
                &one_local_winner
            ),
            PresentedLocalOutcome::Victory
        );

        let one_local_loser = [
            presented_fighter(0, 0, false),
            presented_fighter(1, 1, true),
            None,
            None,
        ];
        assert_eq!(
            local_outcome(
                PresentedMatchOutcome::FighterWinner(winner),
                &one_local_loser
            ),
            PresentedLocalOutcome::Defeat
        );

        let split_couch = [
            presented_fighter(0, 0, true),
            presented_fighter(1, 1, true),
            None,
            None,
        ];
        assert_eq!(
            local_outcome(PresentedMatchOutcome::FighterWinner(winner), &split_couch),
            PresentedLocalOutcome::Mixed
        );
        assert_eq!(
            local_outcome(PresentedMatchOutcome::Draw, &split_couch),
            PresentedLocalOutcome::Draw
        );
        assert_eq!(
            local_outcome(
                PresentedMatchOutcome::Aborted(PresentedAbortReason::SessionFailure),
                &split_couch
            ),
            PresentedLocalOutcome::NoContest
        );

        let no_local = [
            presented_fighter(0, 0, false),
            presented_fighter(1, 1, false),
            None,
            None,
        ];
        assert_eq!(
            local_outcome(PresentedMatchOutcome::FighterWinner(winner), &no_local),
            PresentedLocalOutcome::NoContest
        );
    }

    #[test]
    fn result_sfx_history_deduplicates_by_confirmed_key() {
        let match_id = MatchId::new([9; 16]).unwrap();
        let first = ConfirmedResultKey {
            match_id,
            result_id: 1,
        };
        let second = ConfirmedResultKey {
            match_id,
            result_id: 2,
        };
        let mut history = PresentedResultSfxHistory::default();
        assert!(history.mark_if_new(first));
        assert!(!history.mark_if_new(first));
        assert!(history.mark_if_new(second));
    }
}
