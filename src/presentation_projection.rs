//! Projection boundary from a render-free predicted world into the client app.
//!
//! Online play owns three distinct worlds: authority, client prediction, and
//! rendering. This module is the one-way bridge from prediction to rendering.
//! It restores canonical snapshots into the rendering world's simulation
//! proxies, copies bounded semantic events and their render-only sidecars, and
//! rewinds speculative presentation work after rollback. The rendering world
//! is required to have its local canonical schedule disabled.

use bevy::prelude::*;
use std::cell::Cell;
use std::error::Error;
use std::fmt;
use std::rc::Rc;

use crate::arena::{
    ArenaFighterBurn, ArenaPresentationIntent, ArenaPresentationIntentJournal,
    bootstrap_canonical_arena_runtime,
};
use crate::arena_defs::arena_definitions;
use crate::bee_skills::{BeePresentationIntent, BeePresentationIntentJournal};
use crate::chick_skills::{ChickPresentationIntent, ChickPresentationIntentJournal};
use crate::combat::{
    CombatPresentationCueIntent, CombatPresentationDispatchHistory, CombatPresentationIntent,
    CombatPresentationIntentJournal, HitboxSceneVisual,
};
use crate::components::{Fighter, FighterActionState, FighterStats, Hitbox, SimPosition};
use crate::ecs_identity::{SimulationIdentityAllocator, StableSimEntity};
use crate::effects::VisualEffect;
use crate::fighter::{FighterPresentationIntent, FighterPresentationIntentJournal};
use crate::headless::snapshot_contract_for_manifest;
use crate::interpolation::{SimPoseHistory, SimPoseSnapRequest};
use crate::items::{ItemPresentationIntent, ItemPresentationIntentJournal};
use crate::live_authority::{LiveSimulationDriver, LiveSimulationError};
use crate::live_world_snapshot::LiveWorldSnapshotAdapter;
use crate::match_presentation::{ConfirmedMatchPresentation, MatchPresentationTransient};
use crate::network_protocol::{MatchManifest, ProtocolValidationError};
use crate::penguin_skills::{PenguinPresentationIntent, PenguinPresentationIntentJournal};
use crate::rollback::RollbackEventDiscard;
use crate::sim_event::{
    EventEmitError, PresentationEventCursor, PresentationEventRouter, SimEvent, SimEventJournal,
    TickEventBuffer,
};
use crate::simulation::{SimTick, SimulationDriveMode};
use crate::snapshot::CanonicalSnapshot;
use crate::snapshot_ecs::SnapshotContract;
use crate::snapshot_ecs::{EcsSnapshotError, EcsSnapshotRestoreReport};
use crate::specials::{SpecialPresentationIntent, SpecialPresentationIntentJournal};
use crate::{combat::HitEffects, game_state::MatchAnnouncements};

/// Authority confirmation frontier used by presentation policy routing.
///
/// Offline rendering does not install this resource and treats its newest local
/// event tick as confirmed. An online projector installs it before the first
/// snapshot and advances it monotonically from authority acknowledgements.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PresentationAuthorityFrontier {
    confirmed_through: Option<SimTick>,
}

impl PresentationAuthorityFrontier {
    pub const fn confirmed_through(self) -> Option<SimTick> {
        self.confirmed_through
    }

    fn observe(&mut self, tick: Option<SimTick>) {
        if let Some(tick) = tick {
            self.confirmed_through = Some(
                self.confirmed_through
                    .map_or(tick, |current| current.max(tick)),
            );
        }
    }
}

/// Cloneable rollback hook shared with [`crate::predicted_client::PredictedClient`].
/// Multiple corrections before one render projection retain the earliest tick.
#[derive(Clone, Debug, Default)]
pub struct LiveRollbackPresentationHooks {
    pending_retain_through: Rc<Cell<Option<SimTick>>>,
}

impl LiveRollbackPresentationHooks {
    pub fn pending_retain_through(&self) -> Option<SimTick> {
        self.pending_retain_through.get()
    }

    fn clear(&self) {
        self.pending_retain_through.set(None);
    }
}

impl RollbackEventDiscard for LiveRollbackPresentationHooks {
    fn discard_after(&mut self, retained_through: SimTick) {
        let retained_through = self
            .pending_retain_through
            .get()
            .map_or(retained_through, |pending| pending.min(retained_through));
        self.pending_retain_through.set(Some(retained_through));
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LivePresentationProjectionMetrics {
    pub projected_snapshots: u64,
    pub projected_event_ticks: u64,
    pub projected_events: u64,
    pub rollback_rewinds: u64,
    pub missed_source_event_ticks: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LivePresentationProjectionReport {
    pub snapshot_tick: SimTick,
    pub snapshot_hash: u64,
    pub confirmed_through: Option<SimTick>,
    pub rollback_retain_through: Option<SimTick>,
    pub restore: EcsSnapshotRestoreReport,
    pub projected_event_ticks: u64,
    pub projected_events: u64,
}

#[derive(Debug)]
pub enum LivePresentationProjectionError {
    SourceSnapshot(LiveSimulationError),
    TargetSnapshot(EcsSnapshotError),
    Protocol(ProtocolValidationError),
    TargetSimulationStillLocal,
    MissingSourceEventJournal,
    MissingTargetEventJournal,
    EventEmit(EventEmitError),
    EventIdentityChanged,
    RollbackRetainsFuture {
        retained_through: SimTick,
        projected_tick: SimTick,
    },
}

impl fmt::Display for LivePresentationProjectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "live presentation projection failed: {self:?}")
    }
}

impl Error for LivePresentationProjectionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::SourceSnapshot(error) => Some(error),
            Self::TargetSnapshot(error) => Some(error),
            Self::Protocol(error) => Some(error),
            _ => None,
        }
    }
}

impl From<EcsSnapshotError> for LivePresentationProjectionError {
    fn from(error: EcsSnapshotError) -> Self {
        Self::TargetSnapshot(error)
    }
}

impl From<EventEmitError> for LivePresentationProjectionError {
    fn from(error: EventEmitError) -> Self {
        Self::EventEmit(error)
    }
}

/// Stateful, bounded projection cursor for one online client match.
pub struct LivePresentationProjector {
    snapshots: LiveWorldSnapshotAdapter,
    rollback: LiveRollbackPresentationHooks,
    last_snapshot_tick: Option<SimTick>,
    last_snapshot_hash: Option<u64>,
    last_event_tick: Option<SimTick>,
    metrics: LivePresentationProjectionMetrics,
}

impl Default for LivePresentationProjector {
    fn default() -> Self {
        Self::new()
    }
}

impl LivePresentationProjector {
    pub fn new() -> Self {
        Self {
            snapshots: LiveWorldSnapshotAdapter::new(),
            rollback: LiveRollbackPresentationHooks::default(),
            last_snapshot_tick: None,
            last_snapshot_hash: None,
            last_event_tick: None,
            metrics: LivePresentationProjectionMetrics::default(),
        }
    }

    pub fn rollback_hooks(&self) -> LiveRollbackPresentationHooks {
        self.rollback.clone()
    }

    pub const fn metrics(&self) -> LivePresentationProjectionMetrics {
        self.metrics
    }

    /// Converts an already-bootstrapped rendered world into an online
    /// projection target. Calling this at match entry also clears any event or
    /// intent history left by a previous local match.
    pub fn prepare_target(
        &mut self,
        target: &mut World,
        manifest: &MatchManifest,
    ) -> Result<(), LivePresentationProjectionError> {
        manifest
            .validate()
            .map_err(LivePresentationProjectionError::Protocol)?;
        let arena_index = usize::from(manifest.arena.get());
        if arena_index >= arena_definitions().len() {
            return Err(LivePresentationProjectionError::Protocol(
                ProtocolValidationError::InvalidManifest,
            ));
        }
        clear_projection_match_state(target);
        bootstrap_canonical_arena_runtime(target, arena_index);
        target.insert_resource(snapshot_contract_for_manifest(manifest));
        target.insert_resource(SimulationDriveMode::ExternalProjection);
        target.insert_resource(SimEventJournal::default());
        target.insert_resource(PresentationEventCursor::default());
        target.insert_resource(PresentationEventRouter::default());
        target.insert_resource(CombatPresentationIntentJournal::default());
        target.insert_resource(CombatPresentationDispatchHistory::default());
        target.insert_resource(FighterPresentationIntentJournal::default());
        target.insert_resource(ItemPresentationIntentJournal::default());
        target.insert_resource(ArenaPresentationIntentJournal::default());
        target.insert_resource(SpecialPresentationIntentJournal::default());
        target.insert_resource(BeePresentationIntentJournal::default());
        target.insert_resource(ChickPresentationIntentJournal::default());
        target.insert_resource(PenguinPresentationIntentJournal::default());
        target.insert_resource(PresentationAuthorityFrontier::default());
        target.init_resource::<SimPoseSnapRequest>();

        self.rollback.clear();
        self.last_snapshot_tick = None;
        self.last_snapshot_hash = None;
        self.last_event_tick = None;
        self.metrics = LivePresentationProjectionMetrics::default();
        Ok(())
    }

    /// Captures and projects the latest settled predicted state.
    pub fn project_driver(
        &mut self,
        source: &LiveSimulationDriver,
        target: &mut World,
        confirmed_through: Option<SimTick>,
    ) -> Result<LivePresentationProjectionReport, LivePresentationProjectionError> {
        let snapshot = source
            .capture_live_snapshot()
            .map_err(LivePresentationProjectionError::SourceSnapshot)?;
        self.project_snapshot(source.world(), &snapshot, target, confirmed_through)
    }

    /// Projects an explicitly captured predicted snapshot and its matching event
    /// history. `source` and `snapshot` must describe the same settled tick.
    pub fn project_snapshot(
        &mut self,
        source: &World,
        snapshot: &CanonicalSnapshot,
        target: &mut World,
        confirmed_through: Option<SimTick>,
    ) -> Result<LivePresentationProjectionReport, LivePresentationProjectionError> {
        if target
            .get_resource::<SimulationDriveMode>()
            .is_none_or(|mode| *mode != SimulationDriveMode::ExternalProjection)
        {
            return Err(LivePresentationProjectionError::TargetSimulationStillLocal);
        }

        let snapshot_hash = snapshot
            .canonical_hash()
            .map_err(EcsSnapshotError::Snapshot)
            .map_err(LivePresentationProjectionError::TargetSnapshot)?;
        let rollback_retain_through = self
            .rollback
            .pending_retain_through()
            .or_else(|| self.implicit_rewind_tick(snapshot.header.tick, snapshot_hash));
        if let Some(retained_through) = rollback_retain_through
            && retained_through > snapshot.header.tick
        {
            return Err(LivePresentationProjectionError::RollbackRetainsFuture {
                retained_through,
                projected_tick: snapshot.header.tick,
            });
        }

        // Canonical restore is atomic. Do not consume the rollback notification
        // or mutate presentation queues until the target accepted the snapshot.
        let restore = self.snapshots.restore(target, snapshot)?;

        let correction = rollback_retain_through.is_some() || self.last_snapshot_tick.is_none();
        if let Some(retained_through) = rollback_retain_through {
            discard_target_presentation_after(target, retained_through);
            self.last_event_tick = Some(retained_through);
            self.metrics.rollback_rewinds = self.metrics.rollback_rewinds.saturating_add(1);
        }
        update_projected_pose_history(target, correction);

        let (event_ticks, events) = self.copy_new_events(source, target)?;
        target
            .resource_mut::<PresentationAuthorityFrontier>()
            .observe(confirmed_through);

        self.rollback.clear();
        self.last_snapshot_tick = Some(snapshot.header.tick);
        self.last_snapshot_hash = Some(snapshot_hash);
        self.metrics.projected_snapshots = self.metrics.projected_snapshots.saturating_add(1);
        self.metrics.projected_event_ticks = self
            .metrics
            .projected_event_ticks
            .saturating_add(event_ticks);
        self.metrics.projected_events = self.metrics.projected_events.saturating_add(events);

        Ok(LivePresentationProjectionReport {
            snapshot_tick: snapshot.header.tick,
            snapshot_hash,
            confirmed_through,
            rollback_retain_through,
            restore,
            projected_event_ticks: event_ticks,
            projected_events: events,
        })
    }

    fn implicit_rewind_tick(&self, tick: SimTick, hash: u64) -> Option<SimTick> {
        let previous_tick = self.last_snapshot_tick?;
        if tick < previous_tick {
            return Some(tick);
        }
        if tick == previous_tick && self.last_snapshot_hash.is_some_and(|old| old != hash) {
            return Some(SimTick(tick.0.saturating_sub(1)));
        }
        None
    }

    fn copy_new_events(
        &mut self,
        source: &World,
        target: &mut World,
    ) -> Result<(u64, u64), LivePresentationProjectionError> {
        let source_journal = source
            .get_resource::<SimEventJournal>()
            .ok_or(LivePresentationProjectionError::MissingSourceEventJournal)?;
        if !target.contains_resource::<SimEventJournal>() {
            return Err(LivePresentationProjectionError::MissingTargetEventJournal);
        }
        let (Some(oldest), Some(newest)) =
            (source_journal.oldest_tick(), source_journal.newest_tick())
        else {
            return Ok((0, 0));
        };

        let requested = self
            .last_event_tick
            .and_then(|tick| tick.0.checked_add(1).map(SimTick))
            .unwrap_or(oldest);
        let start = requested.max(oldest);
        if requested < oldest {
            self.metrics.missed_source_event_ticks = self
                .metrics
                .missed_source_event_ticks
                .saturating_add(oldest.0 - requested.0);
        }
        if start > newest {
            return Ok((0, 0));
        }

        let source_combat = source.get_resource::<CombatPresentationIntentJournal>();
        let source_fighter = source.get_resource::<FighterPresentationIntentJournal>();
        let source_item = source.get_resource::<ItemPresentationIntentJournal>();
        let source_arena = source.get_resource::<ArenaPresentationIntentJournal>();
        let source_special = source.get_resource::<SpecialPresentationIntentJournal>();
        let source_bee = source.get_resource::<BeePresentationIntentJournal>();
        let source_chick = source.get_resource::<ChickPresentationIntentJournal>();
        let source_penguin = source.get_resource::<PenguinPresentationIntentJournal>();
        let mut projected_ticks = 0_u64;
        let mut projected_events = 0_u64;
        let mut tick = start;
        loop {
            let records = source_journal.events_at(tick).map(|events| {
                events
                    .iter()
                    .flatten()
                    .copied()
                    .map(|event| ProjectedEventRecord {
                        event,
                        combat: source_combat
                            .as_ref()
                            .and_then(|journal| journal.get(event.id)),
                        combat_cue: source_combat
                            .as_ref()
                            .and_then(|journal| journal.cue(event.id)),
                        fighter: source_fighter
                            .as_ref()
                            .and_then(|journal| journal.get(event.id)),
                        item: source_item
                            .as_ref()
                            .and_then(|journal| journal.get(event.id)),
                        arena: source_arena
                            .as_ref()
                            .and_then(|journal| journal.get(event.id)),
                        special: source_special
                            .as_ref()
                            .and_then(|journal| journal.get(event.id)),
                        bee: source_bee
                            .as_ref()
                            .and_then(|journal| journal.get(event.id)),
                        chick: source_chick
                            .as_ref()
                            .and_then(|journal| journal.get(event.id)),
                        penguin: source_penguin
                            .as_ref()
                            .and_then(|journal| journal.get(event.id)),
                    })
                    .collect::<Vec<_>>()
            });

            let Some(records) = records else {
                self.metrics.missed_source_event_ticks =
                    self.metrics.missed_source_event_ticks.saturating_add(1);
                self.last_event_tick = Some(tick);
                if tick == newest {
                    break;
                }
                tick = SimTick(tick.0.saturating_add(1));
                continue;
            };

            let mut buffer = TickEventBuffer::new(tick);
            for record in &records {
                let emitted = buffer.emit(record.event.id.source, record.event.kind)?;
                if emitted != record.event.id {
                    return Err(LivePresentationProjectionError::EventIdentityChanged);
                }
            }
            target.resource_mut::<SimEventJournal>().commit(&buffer);
            copy_combat_intents(target, &records);
            copy_fighter_intents(target, &records);
            copy_item_intents(target, &records);
            copy_arena_intents(target, &records);
            copy_special_intents(target, &records);
            copy_bee_intents(target, &records);
            copy_chick_intents(target, &records);
            copy_penguin_intents(target, &records);

            projected_ticks = projected_ticks.saturating_add(1);
            projected_events = projected_events.saturating_add(records.len() as u64);
            self.last_event_tick = Some(tick);
            if tick == newest {
                break;
            }
            tick = SimTick(tick.0.saturating_add(1));
        }
        Ok((projected_ticks, projected_events))
    }
}

/// Immediately releases all match-scoped projection state while preserving the
/// long-lived rendered scene (fighter visual hierarchies, arena geometry, HUD,
/// cameras, assets) and confirmed progression ledger.
pub fn release_projection_target(target: &mut World) {
    clear_projection_match_state(target);
    target.insert_resource(SimulationDriveMode::Local);
    target.remove_resource::<SnapshotContract>();
    target.remove_resource::<PresentationAuthorityFrontier>();
}

fn clear_projection_match_state(target: &mut World) {
    // Stable simulation objects other than fighter proxies are always
    // match-owned. Despawn roots immediately so child SceneRoot hierarchies
    // cannot alias a reused generational ID in the next session.
    let dynamic_roots = {
        let mut query =
            target.query_filtered::<Entity, (With<StableSimEntity>, Without<Fighter>)>();
        query.iter(target).collect::<Vec<_>>()
    };
    for entity in dynamic_roots {
        let _ = target.despawn(entity);
    }

    let presentation_entities = {
        let mut query = target.query_filtered::<Entity, (
            Or<(
                With<MatchPresentationTransient>,
                With<VisualEffect>,
                With<HitboxSceneVisual>,
            )>,
            Without<Fighter>,
        )>();
        query.iter(target).collect::<Vec<_>>()
    };
    for entity in presentation_entities {
        if target.get_entity(entity).is_ok() {
            let _ = target.despawn(entity);
        }
    }

    // Canonical restore intentionally excludes render-only feedback fields.
    // Clear them at the match boundary so a prior match cannot tint or pose a
    // freshly restored fighter.
    let fighter_entities = {
        let mut fighters = target.query_filtered::<(
            Entity,
            &mut FighterStats,
            &mut FighterActionState,
            &mut Transform,
        ), With<Fighter>>();
        fighters
            .iter_mut(target)
            .map(|(entity, mut stats, mut action, mut transform)| {
                stats.hud_flash = 0.0;
                action.clear_reaction_visual();
                transform.rotation = Quat::IDENTITY;
                transform.scale = Vec3::ONE;
                entity
            })
            .collect::<Vec<_>>()
    };
    for entity in fighter_entities {
        if let Ok(mut fighter) = target.get_entity_mut(entity) {
            fighter.remove::<ArenaFighterBurn>();
        }
    }

    target.insert_resource(SimEventJournal::default());
    target.insert_resource(PresentationEventCursor::default());
    target.insert_resource(PresentationEventRouter::default());
    target.insert_resource(CombatPresentationIntentJournal::default());
    target.insert_resource(CombatPresentationDispatchHistory::default());
    target.insert_resource(FighterPresentationIntentJournal::default());
    target.insert_resource(ItemPresentationIntentJournal::default());
    target.insert_resource(ArenaPresentationIntentJournal::default());
    target.insert_resource(SpecialPresentationIntentJournal::default());
    target.insert_resource(BeePresentationIntentJournal::default());
    target.insert_resource(ChickPresentationIntentJournal::default());
    target.insert_resource(PenguinPresentationIntentJournal::default());
    target.insert_resource(HitEffects::default());
    target.insert_resource(MatchAnnouncements::default());
    target.insert_resource(SimPoseSnapRequest::requested());
    target.insert_resource(SimulationIdentityAllocator::default());
    target.remove_resource::<ConfirmedMatchPresentation>();
}

#[derive(Clone, Copy)]
struct ProjectedEventRecord {
    event: SimEvent,
    combat: Option<CombatPresentationIntent>,
    combat_cue: Option<CombatPresentationCueIntent>,
    fighter: Option<FighterPresentationIntent>,
    item: Option<ItemPresentationIntent>,
    arena: Option<ArenaPresentationIntent>,
    special: Option<SpecialPresentationIntent>,
    bee: Option<BeePresentationIntent>,
    chick: Option<ChickPresentationIntent>,
    penguin: Option<PenguinPresentationIntent>,
}

fn copy_combat_intents(target: &mut World, records: &[ProjectedEventRecord]) {
    let mut journal = target.resource_mut::<CombatPresentationIntentJournal>();
    for record in records {
        if let Some(intent) = record.combat {
            let _ = journal.record(intent);
        }
        if let Some(intent) = record.combat_cue {
            let _ = journal.record_cue(intent);
        }
    }
}

fn copy_fighter_intents(target: &mut World, records: &[ProjectedEventRecord]) {
    let mut journal = target.resource_mut::<FighterPresentationIntentJournal>();
    for record in records {
        if let Some(intent) = record.fighter {
            let _ = journal.record(intent);
        }
    }
}

fn copy_item_intents(target: &mut World, records: &[ProjectedEventRecord]) {
    let mut journal = target.resource_mut::<ItemPresentationIntentJournal>();
    for record in records {
        if let Some(intent) = record.item {
            let _ = journal.record(intent);
        }
    }
}

fn copy_arena_intents(target: &mut World, records: &[ProjectedEventRecord]) {
    let mut journal = target.resource_mut::<ArenaPresentationIntentJournal>();
    for record in records {
        if let Some(intent) = record.arena {
            let _ = journal.record(intent);
        }
    }
}

fn copy_special_intents(target: &mut World, records: &[ProjectedEventRecord]) {
    let mut journal = target.resource_mut::<SpecialPresentationIntentJournal>();
    for record in records {
        if let Some(intent) = record.special {
            let _ = journal.record(intent);
        }
    }
}

fn copy_bee_intents(target: &mut World, records: &[ProjectedEventRecord]) {
    let mut journal = target.resource_mut::<BeePresentationIntentJournal>();
    for record in records {
        if let Some(intent) = record.bee {
            let _ = journal.record(intent);
        }
    }
}

fn copy_chick_intents(target: &mut World, records: &[ProjectedEventRecord]) {
    let mut journal = target.resource_mut::<ChickPresentationIntentJournal>();
    for record in records {
        if let Some(intent) = record.chick {
            let _ = journal.record(intent);
        }
    }
}

fn copy_penguin_intents(target: &mut World, records: &[ProjectedEventRecord]) {
    let mut journal = target.resource_mut::<PenguinPresentationIntentJournal>();
    for record in records {
        if let Some(intent) = record.penguin {
            let _ = journal.record(intent);
        }
    }
}

fn discard_target_presentation_after(target: &mut World, retained_through: SimTick) {
    target
        .resource_mut::<SimEventJournal>()
        .discard_after(retained_through);
    RollbackEventDiscard::discard_after(
        &mut *target.resource_mut::<PresentationEventCursor>(),
        retained_through,
    );
    RollbackEventDiscard::discard_after(
        &mut *target.resource_mut::<PresentationEventRouter>(),
        retained_through,
    );
    target
        .resource_mut::<CombatPresentationIntentJournal>()
        .discard_after(retained_through);
    target
        .resource_mut::<FighterPresentationIntentJournal>()
        .discard_after(retained_through);
    target
        .resource_mut::<ItemPresentationIntentJournal>()
        .discard_after(retained_through);
    target
        .resource_mut::<ArenaPresentationIntentJournal>()
        .discard_after(retained_through);
    target
        .resource_mut::<SpecialPresentationIntentJournal>()
        .discard_after(retained_through);
    target
        .resource_mut::<BeePresentationIntentJournal>()
        .discard_after(retained_through);
    target
        .resource_mut::<ChickPresentationIntentJournal>()
        .discard_after(retained_through);
    target
        .resource_mut::<PenguinPresentationIntentJournal>()
        .discard_after(retained_through);

    // Snapshot restore runs before this cleanup. A speculative surface is kept
    // when the corrected canonical world still owns the same generational
    // hitbox ID; otherwise remove it immediately instead of letting a rejected
    // prediction linger for its render-local fade. The dedup history itself is
    // intentionally retained, matching all other one-shot presentation.
    let live_hitboxes = {
        let mut query = target.query_filtered::<(&StableSimEntity, &Hitbox), With<Hitbox>>();
        query
            .iter(target)
            .map(|(stable, hitbox)| (stable.id(), hitbox.elapsed.as_seconds()))
            .collect::<Vec<_>>()
    };
    let rejected_surfaces = {
        let mut query = target.query::<(Entity, &mut HitboxSceneVisual)>();
        query
            .iter_mut(target)
            .filter_map(|(entity, mut visual)| {
                if let Some((_, elapsed)) = live_hitboxes
                    .iter()
                    .find(|(id, _)| *id == visual.surface.entity())
                {
                    visual.rewind_to_canonical_elapsed(*elapsed);
                    None
                } else {
                    (visual.spawn_tick > retained_through).then_some(entity)
                }
            })
            .collect::<Vec<_>>()
    };
    for entity in rejected_surfaces {
        target.despawn(entity);
    }
}

fn update_projected_pose_history(target: &mut World, snap: bool) {
    let mut fighters = target.query::<(&SimPosition, &mut SimPoseHistory)>();
    for (position, mut history) in fighters.iter_mut(target) {
        if snap {
            history.snap(position.translation);
        } else {
            history.begin_tick();
            history.capture(position.translation);
        }
    }
    if snap && let Some(mut request) = target.get_resource_mut::<SimPoseSnapRequest>() {
        request.request();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arena_defs::ActiveArena;
    use crate::bee_skills::BeePresentationKind;
    use crate::combat::ImpactSource;
    use crate::confirmed_progression::ConfirmedProgressionLedger;
    use crate::determinism::{SimEntityId, SimEntityKind};
    use crate::effects::FeedbackPackageId;
    use crate::game_state::LocalSetup;
    use crate::headless::build_headless_simulation;
    use crate::match_config::{MatchBuildOptions, build_headless_match_config};
    use crate::network_protocol::{AuthorityKind, DefinitionId, MatchId, PeerId};
    use crate::sim_event::{AbilityLifecycleEvent, SimEventId, SimEventKind, SimEventSource};
    use crate::specials::SpecialPresentationKind;

    #[derive(Component)]
    struct StaticPresentationProbe;

    fn config_for_arena(arena_index: usize) -> crate::headless::HeadlessMatchConfig {
        let mut setup = LocalSetup::default();
        setup.arena_index = arena_index;
        let peer = PeerId::new(71).unwrap();
        build_headless_match_config(
            &setup,
            MatchBuildOptions::single_peer(
                MatchId::new([0x71; 16]).unwrap(),
                AuthorityKind::Listen,
                false,
                peer,
                &setup,
                SimTick(120),
            ),
        )
        .unwrap()
    }

    #[test]
    fn every_manifest_arena_bootstraps_before_first_projection() {
        let mut target = build_headless_simulation(config_for_arena(0)).unwrap();
        for arena_index in 0..arena_definitions().len() {
            let config = config_for_arena(arena_index);
            let manifest = config.manifest;
            let source = build_headless_simulation(config).unwrap();
            let mut projector = LivePresentationProjector::new();
            projector
                .prepare_target(target.world_mut(), &manifest)
                .unwrap();
            assert_eq!(
                target.world().resource::<ActiveArena>().index(),
                arena_index
            );
            projector
                .project_driver(&source, target.world_mut(), Some(SimTick::ZERO))
                .unwrap();
            assert_eq!(target.state_hash().unwrap(), source.state_hash().unwrap());
        }
    }

    #[test]
    fn out_of_catalog_manifest_arena_is_rejected_before_cleanup() {
        let mut world = World::new();
        let survivor = world.spawn(StaticPresentationProbe).id();
        let mut manifest = config_for_arena(0).manifest;
        manifest.arena = DefinitionId::new(arena_definitions().len() as u16).unwrap();
        let error = LivePresentationProjector::new()
            .prepare_target(&mut world, &manifest)
            .unwrap_err();
        assert!(matches!(
            error,
            LivePresentationProjectionError::Protocol(ProtocolValidationError::InvalidManifest)
        ));
        assert!(world.get_entity(survivor).is_ok());
    }

    #[test]
    fn match_boundary_cleanup_prevents_cross_kind_stable_id_aliasing() {
        let mut world = World::new();
        world.init_resource::<SimulationIdentityAllocator>();
        world.insert_resource(ConfirmedProgressionLedger::default());
        let static_scene = world.spawn(StaticPresentationProbe).id();
        let stale_transient = world.spawn(MatchPresentationTransient).id();

        let old_root = world.spawn_empty().id();
        let old_stable = world
            .resource_mut::<SimulationIdentityAllocator>()
            .try_allocate(SimEntityKind::Special, old_root)
            .unwrap();
        world.entity_mut(old_root).insert(old_stable);

        let config = config_for_arena(0);
        LivePresentationProjector::new()
            .prepare_target(&mut world, &config.manifest)
            .unwrap();
        assert!(world.get_entity(old_root).is_err());
        assert!(world.get_entity(stale_transient).is_err());
        assert!(world.get_entity(static_scene).is_ok());
        assert!(world.contains_resource::<ConfirmedProgressionLedger>());

        let new_root = world.spawn_empty().id();
        let new_stable = world
            .resource_mut::<SimulationIdentityAllocator>()
            .try_allocate(SimEntityKind::Item, new_root)
            .unwrap();
        world.entity_mut(new_root).insert(new_stable);
        assert_eq!(old_stable.id().index(), new_stable.id().index());
        assert_eq!(old_stable.id().generation(), new_stable.id().generation());
        assert_ne!(old_stable.id().kind(), new_stable.id().kind());

        release_projection_target(&mut world);
        assert!(world.get_entity(new_root).is_err());
        assert!(world.get_entity(static_scene).is_ok());
        assert_eq!(
            *world.resource::<SimulationDriveMode>(),
            SimulationDriveMode::Local
        );
    }

    #[test]
    fn rollback_hook_keeps_the_earliest_unprojected_restore() {
        let projector = LivePresentationProjector::new();
        let mut hook = projector.rollback_hooks();
        hook.discard_after(SimTick(18));
        hook.discard_after(SimTick(21));
        hook.discard_after(SimTick(12));
        assert_eq!(
            projector.rollback.pending_retain_through(),
            Some(SimTick(12))
        );
    }

    #[test]
    fn ability_sidecars_copy_and_rewind_with_projected_events() {
        let special_entity = SimEntityId::new(SimEntityKind::Special, 0, 1);
        let bee_entity = SimEntityId::new(SimEntityKind::BeeSkill, 0, 1);
        let special_event = SimEvent {
            id: SimEventId {
                tick: SimTick(14),
                source: SimEventSource::Entity(special_entity),
                ordinal: 0,
            },
            kind: SimEventKind::AbilityLifecycle {
                entity: special_entity,
                event: AbilityLifecycleEvent::Spawned,
            },
        };
        let bee_event = SimEvent {
            id: SimEventId {
                tick: SimTick(14),
                source: SimEventSource::Entity(bee_entity),
                ordinal: 1,
            },
            kind: SimEventKind::AbilityLifecycle {
                entity: bee_entity,
                event: AbilityLifecycleEvent::Spawned,
            },
        };
        let special_intent = SpecialPresentationIntent {
            event_id: special_event.id,
            entity: special_entity,
            kind: SpecialPresentationKind::Lifecycle {
                event: AbilityLifecycleEvent::Spawned,
                position: Vec3::ZERO,
                direction: Vec3::Z,
                package: Some(FeedbackPackageId::SpecialProjectileStartup),
                cue: None,
                source: ImpactSource::Projectile,
                priority: 0,
            },
        };
        let bee_intent = BeePresentationIntent {
            event_id: bee_event.id,
            entity: bee_entity,
            kind: BeePresentationKind::Lifecycle {
                event: AbilityLifecycleEvent::Spawned,
                position: Vec3::ZERO,
                direction: Vec3::X,
                package: Some(FeedbackPackageId::SpecialProjectileStartup),
                cue: None,
                source: ImpactSource::Projectile,
                priority: 0,
            },
        };
        let records = vec![
            ProjectedEventRecord {
                event: special_event,
                combat: None,
                combat_cue: None,
                fighter: None,
                item: None,
                arena: None,
                special: Some(special_intent),
                bee: None,
                chick: None,
                penguin: None,
            },
            ProjectedEventRecord {
                event: bee_event,
                combat: None,
                combat_cue: None,
                fighter: None,
                item: None,
                arena: None,
                special: None,
                bee: Some(bee_intent),
                chick: None,
                penguin: None,
            },
        ];
        let mut target = World::new();
        target.init_resource::<SpecialPresentationIntentJournal>();
        target.init_resource::<BeePresentationIntentJournal>();

        copy_special_intents(&mut target, &records);
        copy_bee_intents(&mut target, &records);
        assert_eq!(
            target
                .resource::<SpecialPresentationIntentJournal>()
                .get(special_event.id),
            Some(special_intent)
        );
        assert_eq!(
            target
                .resource::<BeePresentationIntentJournal>()
                .get(bee_event.id),
            Some(bee_intent)
        );

        target
            .resource_mut::<SpecialPresentationIntentJournal>()
            .discard_after(SimTick(13));
        target
            .resource_mut::<BeePresentationIntentJournal>()
            .discard_after(SimTick(13));
        assert!(
            target
                .resource::<SpecialPresentationIntentJournal>()
                .get(special_event.id)
                .is_none()
        );
        assert!(
            target
                .resource::<BeePresentationIntentJournal>()
                .get(bee_event.id)
                .is_none()
        );
    }
}
