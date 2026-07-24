//! Deterministic simulation events and rollback-safe presentation consumption.
//!
//! Simulation code emits intent through this module instead of retaining render,
//! audio, camera, platform, or progression objects. Event IDs are reproducible
//! across prediction and re-simulation. All queues are fixed-capacity and reject
//! newest work on overflow so a bad match can never grow memory without bound.

use bevy::prelude::{Res, ResMut, Resource};
use std::hash::{Hash, Hasher};

use crate::determinism::{FighterId, SimEntityId};
use crate::reactions::ReactionFamilyId;
use crate::rollback::RollbackEventDiscard;
use crate::simulation::SimTick;

pub const MAX_SIM_EVENTS_PER_TICK: usize = 256;
pub const SIM_EVENT_HISTORY_TICKS: usize = 128;
/// Deduplication covers the complete canonical event-journal horizon. A normal
/// twelve-tick rollback at the per-tick event cap therefore cannot evict an ID
/// before the corrected timeline has replayed it.
pub const PRESENTED_EVENT_HISTORY: usize = SIM_EVENT_HISTORY_TICKS * MAX_SIM_EVENTS_PER_TICK;
/// Confirmation may lag as far as the retained event journal. The queue uses
/// heap-backed fixed storage so the hostile-but-valid upper bound does not grow
/// the stack or allocate in response to match traffic.
pub const MAX_PENDING_CONFIRMED_EVENTS: usize = PRESENTED_EVENT_HISTORY;
/// One canonical timeline can contribute at most 256 IDs to a tick. A second
/// fixed bank retains corrected full IDs that reuse an ordinal with a different
/// source. Further hostile correction churn is rejected fail-closed (the new
/// presentation is suppressed) rather than growing memory or replaying an old
/// one-shot.
const MAX_PRESENTED_IDS_PER_TICK: usize = MAX_SIM_EVENTS_PER_TICK * 2;
const PRESENTED_ID_STORAGE: usize = SIM_EVENT_HISTORY_TICKS * MAX_PRESENTED_IDS_PER_TICK;

/// Stable origin used as part of a deterministic [`SimEventId`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SimEventSource {
    Match,
    Arena,
    Fighter(FighterId),
    Entity(SimEntityId),
    /// One immutable hazard within one immutable arena definition. Static
    /// hazards have no generational entity slot and must not share the generic
    /// arena event identity when several hazards emit on the same tick.
    ArenaHazard {
        arena_index: u16,
        hazard_index: u16,
    },
}

/// Identity for one event emitted by one source during one simulation tick.
///
/// `ordinal` is assigned by the tick-local buffer in canonical emission order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SimEventId {
    pub tick: SimTick,
    pub source: SimEventSource,
    pub ordinal: u16,
}

/// Authoritative fighter-state transitions that may have render-local
/// lifecycle presentation attached to them.
///
/// These values intentionally describe simulation facts rather than particles,
/// sounds, camera shakes, or authored cue names. The latter live in the bounded
/// fighter presentation sidecar and never enter snapshots or network payloads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FighterLifecycleEvent {
    DrunkBubble,
    DashTrail,
    RecoveryStarted,
    RecoveryCompleted,
    WallBounced,
    Landed,
    LandingAftermath,
    GroundBounced,
    KnockdownLanded,
    RingOut,
    Knockout,
}

/// Authoritative item transitions that may have render-local feedback attached.
///
/// The payload deliberately names gameplay facts, not particles, audio cues, or
/// announcement strings. Renderer-facing data is paired by [`SimEventId`] in
/// the bounded item presentation journal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemLifecycleEvent {
    PickedUp,
    Thrown,
    Used,
    Dropped,
    Broken,
    CrateOpened,
    AlcoholSprayed,
    Exploded,
}

/// Authoritative lifecycle phases shared by stable fighter-ability entities.
///
/// The phase is intentionally renderer-agnostic. Authored packages, cue names,
/// positions used for accents, and camera/audio choices live in bounded
/// module-specific presentation journals keyed by the resulting event ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AbilityLifecycleEvent {
    Spawned,
    Activated,
    Repeated,
    Aftermath,
    Despawned,
}

/// Canonical match-clock transitions with client presentation attached.
///
/// Announcement text, audio cues, and camera feedback deliberately do not live
/// here. Clients derive those local details after prediction/rollback has
/// settled, while an authority records only the phase fact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchLifecycleEvent {
    TimeUp,
    Results,
}

/// Compact authoritative event payload. Presentation may enrich these IDs with
/// local assets, but those assets can never feed back into simulation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SimEventKind {
    ActionStarted {
        fighter: FighterId,
        action_id: u16,
    },
    HitConfirmed {
        /// `None` denotes a neutral arena or match-owned source.
        attacker: Option<FighterId>,
        victim: FighterId,
        /// Committed health damage in the simulation's canonical 1/4096 units.
        damage_q: i32,
        reaction: ReactionFamilyId,
    },
    Guarded {
        /// `None` denotes a neutral arena or match-owned source.
        attacker: Option<FighterId>,
        defender: FighterId,
    },
    EntitySpawned {
        entity: SimEntityId,
    },
    EntityDespawned {
        entity: SimEntityId,
    },
    StockLost {
        fighter: FighterId,
        stocks_remaining: u8,
    },
    FighterRespawned {
        fighter: FighterId,
    },
    FighterLifecycle {
        fighter: FighterId,
        event: FighterLifecycleEvent,
    },
    ItemLifecycle {
        item: SimEntityId,
        fighter: Option<FighterId>,
        event: ItemLifecycleEvent,
    },
    AbilityLifecycle {
        entity: SimEntityId,
        event: AbilityLifecycleEvent,
    },
    MatchLifecycle {
        event: MatchLifecycleEvent,
    },
    MatchResult {
        winner: Option<FighterId>,
        result_id: u64,
    },
    Statistic {
        fighter: FighterId,
        statistic_id: u16,
        delta: i32,
    },
}

/// Controls when an event may create irreversible presentation or platform work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresentationPolicy {
    /// Can be shown on predicted state and may be corrected visually.
    Predicted,
    /// Can be shown predicted, but its stable ID must be consumed at most once.
    PredictedDeduplicated,
    /// Must wait until the authority confirms the event's tick.
    ConfirmedOnly,
}

impl SimEventKind {
    pub const fn presentation_policy(self) -> PresentationPolicy {
        match self {
            Self::ActionStarted { .. } => PresentationPolicy::Predicted,
            Self::HitConfirmed { .. }
            | Self::Guarded { .. }
            | Self::EntitySpawned { .. }
            | Self::EntityDespawned { .. }
            | Self::FighterRespawned { .. }
            | Self::FighterLifecycle { .. }
            | Self::ItemLifecycle { .. }
            | Self::AbilityLifecycle { .. }
            | Self::MatchLifecycle {
                event: MatchLifecycleEvent::TimeUp,
            } => PresentationPolicy::PredictedDeduplicated,
            Self::StockLost { .. }
            | Self::MatchLifecycle {
                event: MatchLifecycleEvent::Results,
            }
            | Self::MatchResult { .. }
            | Self::Statistic { .. } => PresentationPolicy::ConfirmedOnly,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SimEvent {
    pub id: SimEventId,
    pub kind: SimEventKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventEmitError {
    CapacityExceeded { capacity: usize },
    OrdinalExhausted,
}

/// Fixed-capacity event list for one simulation tick.
#[derive(Resource, Clone, Debug)]
pub struct TickEventBuffer {
    tick: SimTick,
    len: u16,
    events: [Option<SimEvent>; MAX_SIM_EVENTS_PER_TICK],
    overflow_count: u32,
}

impl Default for TickEventBuffer {
    fn default() -> Self {
        Self::new(SimTick::ZERO)
    }
}

impl TickEventBuffer {
    pub const fn new(tick: SimTick) -> Self {
        Self {
            tick,
            len: 0,
            events: [None; MAX_SIM_EVENTS_PER_TICK],
            overflow_count: 0,
        }
    }

    pub fn begin_tick(&mut self, tick: SimTick) {
        self.tick = tick;
        self.len = 0;
    }

    pub const fn tick(&self) -> SimTick {
        self.tick
    }

    pub const fn len(&self) -> usize {
        self.len as usize
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub const fn overflow_count(&self) -> u32 {
        self.overflow_count
    }

    pub fn emit(
        &mut self,
        source: SimEventSource,
        kind: SimEventKind,
    ) -> Result<SimEventId, EventEmitError> {
        let index = self.len();
        if index >= MAX_SIM_EVENTS_PER_TICK {
            self.overflow_count = self.overflow_count.saturating_add(1);
            return Err(EventEmitError::CapacityExceeded {
                capacity: MAX_SIM_EVENTS_PER_TICK,
            });
        }
        let ordinal = u16::try_from(index).map_err(|_| EventEmitError::OrdinalExhausted)?;
        let id = SimEventId {
            tick: self.tick,
            source,
            ordinal,
        };
        self.events[index] = Some(SimEvent { id, kind });
        self.len += 1;
        Ok(id)
    }

    pub fn as_slice(&self) -> &[Option<SimEvent>] {
        &self.events[..self.len()]
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = &SimEvent> {
        self.as_slice().iter().map(|event| {
            event
                .as_ref()
                .expect("active event prefix is always initialized")
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct JournalSlot {
    tick: SimTick,
    len: u16,
    occupied: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SimEventJournalMetrics {
    pub committed_ticks: u64,
    pub committed_events: u64,
    pub overwritten_ticks: u64,
    pub discarded_ticks: u64,
    pub high_water_ticks: u16,
}

/// Bounded canonical event history shared by prediction, rollback, replay, and
/// presentation. Storage is allocated once; committing an empty tick performs
/// no event copies and every lookup is tick-addressed rather than ECS-ordered.
#[derive(Resource, Clone, Debug)]
pub struct SimEventJournal {
    slots: [JournalSlot; SIM_EVENT_HISTORY_TICKS],
    events: Box<[Option<SimEvent>]>,
    len: usize,
    newest: Option<SimTick>,
    metrics: SimEventJournalMetrics,
}

impl Default for SimEventJournal {
    fn default() -> Self {
        Self {
            slots: [JournalSlot::default(); SIM_EVENT_HISTORY_TICKS],
            events: vec![None; SIM_EVENT_HISTORY_TICKS * MAX_SIM_EVENTS_PER_TICK]
                .into_boxed_slice(),
            len: 0,
            newest: None,
            metrics: SimEventJournalMetrics::default(),
        }
    }
}

impl SimEventJournal {
    const fn slot_index(tick: SimTick) -> usize {
        tick.0 as usize % SIM_EVENT_HISTORY_TICKS
    }

    const fn event_offset(slot: usize) -> usize {
        slot * MAX_SIM_EVENTS_PER_TICK
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub const fn newest_tick(&self) -> Option<SimTick> {
        self.newest
    }

    pub const fn metrics(&self) -> SimEventJournalMetrics {
        self.metrics
    }

    pub fn oldest_tick(&self) -> Option<SimTick> {
        self.slots
            .iter()
            .filter(|slot| slot.occupied)
            .map(|slot| slot.tick)
            .min()
    }

    /// Records one completed tick. Re-recording the same tick deterministically
    /// replaces its prior events, while modulo eviction drops only the exact
    /// older tick occupying that fixed slot.
    pub fn commit(&mut self, buffer: &TickEventBuffer) {
        let slot_index = Self::slot_index(buffer.tick());
        let old_slot = self.slots[slot_index];
        let offset = Self::event_offset(slot_index);
        for event in &mut self.events[offset..offset + usize::from(old_slot.len)] {
            *event = None;
        }

        if old_slot.occupied && old_slot.tick != buffer.tick() {
            self.metrics.overwritten_ticks = self.metrics.overwritten_ticks.saturating_add(1);
        } else if !old_slot.occupied {
            self.len += 1;
        }

        for (index, event) in buffer.iter().copied().enumerate() {
            self.events[offset + index] = Some(event);
        }
        self.slots[slot_index] = JournalSlot {
            tick: buffer.tick(),
            len: buffer.len() as u16,
            occupied: true,
        };
        self.newest = Some(
            self.newest
                .map_or(buffer.tick(), |tick| tick.max(buffer.tick())),
        );
        self.metrics.committed_ticks = self.metrics.committed_ticks.saturating_add(1);
        self.metrics.committed_events = self
            .metrics
            .committed_events
            .saturating_add(buffer.len() as u64);
        self.metrics.high_water_ticks = self.metrics.high_water_ticks.max(self.len as u16);
    }

    pub fn events_at(&self, tick: SimTick) -> Option<&[Option<SimEvent>]> {
        let slot_index = Self::slot_index(tick);
        let slot = self.slots[slot_index];
        if !slot.occupied || slot.tick != tick {
            return None;
        }
        let offset = Self::event_offset(slot_index);
        Some(&self.events[offset..offset + usize::from(slot.len)])
    }

    pub fn iter_at(&self, tick: SimTick) -> impl ExactSizeIterator<Item = &SimEvent> {
        self.events_at(tick)
            .unwrap_or_default()
            .iter()
            .map(|event| event.as_ref().expect("journal event prefix is initialized"))
    }

    pub fn discard_after(&mut self, tick: SimTick) {
        for slot_index in 0..SIM_EVENT_HISTORY_TICKS {
            let slot = self.slots[slot_index];
            if !slot.occupied || slot.tick <= tick {
                continue;
            }
            let offset = Self::event_offset(slot_index);
            for event in &mut self.events[offset..offset + usize::from(slot.len)] {
                *event = None;
            }
            self.slots[slot_index] = JournalSlot::default();
            self.len = self.len.saturating_sub(1);
            self.metrics.discarded_ticks = self.metrics.discarded_ticks.saturating_add(1);
        }
        self.newest = self
            .slots
            .iter()
            .filter(|slot| slot.occupied)
            .map(|slot| slot.tick)
            .max();
    }
}

impl RollbackEventDiscard for SimEventJournal {
    fn discard_after(&mut self, tick: SimTick) {
        Self::discard_after(self, tick);
    }
}

/// Starts one deterministic event buffer immediately after the canonical clock
/// advances. The cumulative overflow counter intentionally survives resets.
pub fn begin_sim_event_tick(tick: Res<SimTick>, mut buffer: ResMut<TickEventBuffer>) {
    buffer.begin_tick(*tick);
}

/// Archives the final ordered event list before snapshot/hash capture.
pub fn commit_sim_event_tick(buffer: Res<TickEventBuffer>, mut journal: ResMut<SimEventJournal>) {
    journal.commit(&buffer);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PresentedTickSlot {
    tick: SimTick,
    occupied: bool,
    len: u16,
}

impl Default for PresentedTickSlot {
    fn default() -> Self {
        Self {
            tick: SimTick::ZERO,
            occupied: false,
            len: 0,
        }
    }
}

impl PresentedTickSlot {
    fn event_count(self) -> usize {
        usize::from(self.len)
    }
}

/// Tick-addressed fixed history of already-presented event IDs.
///
/// Ordinals are unique within one canonical timeline, but a rollback correction
/// may reuse `(tick, ordinal)` with a different source. Each tick therefore owns
/// a fixed open-addressed table of complete [`SimEventId`] values. Equality is
/// always full-ID equality; hashing affects placement only.
#[derive(Resource, Clone, Debug)]
pub struct PresentedEventHistory {
    slots: [PresentedTickSlot; SIM_EVENT_HISTORY_TICKS],
    ids: Box<[Option<SimEventId>]>,
    len: usize,
}

impl Default for PresentedEventHistory {
    fn default() -> Self {
        Self {
            slots: [PresentedTickSlot::default(); SIM_EVENT_HISTORY_TICKS],
            ids: vec![None; PRESENTED_ID_STORAGE].into_boxed_slice(),
            len: 0,
        }
    }
}

impl PresentedEventHistory {
    pub const fn len(&self) -> usize {
        self.len
    }

    const fn slot_index(tick: SimTick) -> usize {
        tick.0 as usize % SIM_EVENT_HISTORY_TICKS
    }

    const fn slot_offset(slot: usize) -> usize {
        slot * MAX_PRESENTED_IDS_PER_TICK
    }

    fn table_start(id: SimEventId) -> usize {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        id.hash(&mut hasher);
        hasher.finish() as usize % MAX_PRESENTED_IDS_PER_TICK
    }

    const fn table_index(start: usize, probe: usize) -> usize {
        start.wrapping_add(probe) % MAX_PRESENTED_IDS_PER_TICK
    }

    pub fn contains(&self, id: SimEventId) -> bool {
        if usize::from(id.ordinal) >= MAX_SIM_EVENTS_PER_TICK {
            return false;
        }
        let slot_index = Self::slot_index(id.tick);
        let slot = self.slots[slot_index];
        if !slot.occupied || slot.tick != id.tick {
            return false;
        }
        let offset = Self::slot_offset(slot_index);
        let start = Self::table_start(id);
        for probe in 0..MAX_PRESENTED_IDS_PER_TICK {
            match self.ids[offset + Self::table_index(start, probe)] {
                Some(stored) if stored == id => return true,
                Some(_) => {}
                None => return false,
            }
        }
        false
    }

    /// Returns true exactly once for an event while it remains in history.
    pub fn mark_if_new(&mut self, id: SimEventId) -> bool {
        if usize::from(id.ordinal) >= MAX_SIM_EVENTS_PER_TICK {
            return false;
        }
        let slot_index = Self::slot_index(id.tick);
        let offset = Self::slot_offset(slot_index);
        let slot = &mut self.slots[slot_index];
        if !slot.occupied || slot.tick != id.tick {
            self.len = self.len.saturating_sub(slot.event_count());
            for entry in &mut self.ids[offset..offset + MAX_PRESENTED_IDS_PER_TICK] {
                *entry = None;
            }
            *slot = PresentedTickSlot {
                tick: id.tick,
                occupied: true,
                len: 0,
            };
        }

        let start = Self::table_start(id);
        for probe in 0..MAX_PRESENTED_IDS_PER_TICK {
            let entry = &mut self.ids[offset + Self::table_index(start, probe)];
            match *entry {
                Some(stored) if stored == id => return false,
                Some(_) => {}
                None => {
                    *entry = Some(id);
                    slot.len += 1;
                    self.len += 1;
                    return true;
                }
            }
        }

        // Presentation safety wins under pathological repeated corrections:
        // suppress an untracked one-shot instead of replaying or allocating.
        false
    }
}

/// Stores confirmed-only events until the authority advances the confirmed tick.
#[derive(Resource, Clone, Debug)]
pub struct ConfirmedEventQueue {
    entries: Box<[Option<SimEvent>]>,
    len: usize,
    dropped: u32,
}

impl Default for ConfirmedEventQueue {
    fn default() -> Self {
        Self {
            entries: vec![None; MAX_PENDING_CONFIRMED_EVENTS].into_boxed_slice(),
            len: 0,
            dropped: 0,
        }
    }
}

impl ConfirmedEventQueue {
    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn dropped(&self) -> u32 {
        self.dropped
    }

    pub fn push(&mut self, event: SimEvent) -> Result<(), EventEmitError> {
        if self.len == MAX_PENDING_CONFIRMED_EVENTS {
            self.dropped = self.dropped.saturating_add(1);
            return Err(EventEmitError::CapacityExceeded {
                capacity: MAX_PENDING_CONFIRMED_EVENTS,
            });
        }
        self.entries[self.len] = Some(event);
        self.len += 1;
        Ok(())
    }

    pub fn contains(&self, id: SimEventId) -> bool {
        self.entries[..self.len]
            .iter()
            .flatten()
            .any(|event| event.id == id)
    }

    /// Queues an event unless an earlier prediction pass already queued the
    /// same deterministic identity.
    pub fn push_if_new(&mut self, event: SimEvent) -> Result<bool, EventEmitError> {
        if self.contains(event.id) {
            return Ok(false);
        }
        self.push(event)?;
        Ok(true)
    }

    /// Drains confirmed events in their original canonical order.
    pub fn drain_confirmed(
        &mut self,
        confirmed_through: SimTick,
        mut consume: impl FnMut(SimEvent),
    ) {
        let old_len = self.len;
        let mut retained_len = 0_usize;
        for read_index in 0..old_len {
            let event = self.entries[read_index]
                .take()
                .expect("active confirmed-event prefix is initialized");
            if event.id.tick.0 <= confirmed_through.0 {
                consume(event);
            } else {
                self.entries[retained_len] = Some(event);
                retained_len += 1;
            }
        }
        self.len = retained_len;
    }

    /// Drops speculative queue entries after restoring an older snapshot.
    pub fn discard_after(&mut self, tick: SimTick) {
        let old_len = self.len;
        let mut retained_len = 0_usize;
        for read_index in 0..old_len {
            let event = self.entries[read_index]
                .take()
                .expect("active confirmed-event prefix is initialized");
            if event.id.tick.0 <= tick.0 {
                self.entries[retained_len] = Some(event);
                retained_len += 1;
            }
        }
        self.len = retained_len;
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PresentationEventMetrics {
    pub predicted_dispatched: u64,
    pub deduplicated_dispatched: u64,
    pub duplicate_events_suppressed: u64,
    pub confirmed_events_queued: u64,
    pub confirmed_events_dispatched: u64,
    pub confirmed_queue_overflows: u64,
    pub rollback_queue_discards: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PresentationEventCursorMetrics {
    pub observed_ticks: u64,
    pub observed_events: u64,
    /// Ticks that fell out of the journal before presentation observed them.
    /// This is a client-health signal; authoritative simulation is unaffected.
    pub missed_journal_ticks: u64,
    pub rollback_rewinds: u64,
}

/// Tick cursor used by a render-rate presentation consumer.
///
/// A single render frame may follow zero or many fixed simulation steps. This
/// cursor visits every retained committed tick exactly once in normal play. It
/// is deliberately separate from [`PresentedEventHistory`]: the cursor tracks
/// journal traversal, while the history suppresses rollback replay of one-shot
/// event IDs.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PresentationEventCursor {
    observed_through: Option<SimTick>,
    metrics: PresentationEventCursorMetrics,
}

impl PresentationEventCursor {
    pub const fn observed_through(&self) -> Option<SimTick> {
        self.observed_through
    }

    pub const fn metrics(&self) -> PresentationEventCursorMetrics {
        self.metrics
    }

    /// Routes every journal tick not yet seen by this cursor.
    ///
    /// `confirmed_through` is the authority-confirmed frontier. Passing the
    /// newest local tick gives offline/listen-local play immediate confirmation;
    /// an online predicted client passes only the latest authority-confirmed
    /// tick. Confirmed queued events are delivered before newly observed events
    /// so dispatch order remains chronological across render stalls.
    pub fn route_available(
        &mut self,
        journal: &SimEventJournal,
        router: &mut PresentationEventRouter,
        confirmed_through: Option<SimTick>,
        mut dispatch: impl FnMut(SimEvent),
    ) -> Result<(), EventEmitError> {
        if let Some(confirmed_through) = confirmed_through {
            router.confirm_through(confirmed_through, &mut dispatch);
        }

        let (Some(oldest), Some(newest)) = (journal.oldest_tick(), journal.newest_tick()) else {
            return Ok(());
        };
        let mut next = match self.observed_through {
            None => oldest,
            Some(observed) if observed >= newest => return Ok(()),
            Some(observed) => SimTick(observed.0.saturating_add(1)),
        };
        if next < oldest {
            self.metrics.missed_journal_ticks = self
                .metrics
                .missed_journal_ticks
                .saturating_add(oldest.0.saturating_sub(next.0));
            next = oldest;
        }

        while next <= newest {
            if let Some(events) = journal.events_at(next) {
                for event in events.iter().flatten().copied() {
                    router.observe_predicted(event, &mut dispatch)?;
                    self.metrics.observed_events = self.metrics.observed_events.saturating_add(1);
                }
            } else {
                // A well-formed live journal commits even empty simulation ticks.
                // Treat a hole as missed presentation work and keep the cursor
                // bounded instead of waiting forever on a tick that cannot arrive.
                self.metrics.missed_journal_ticks =
                    self.metrics.missed_journal_ticks.saturating_add(1);
            }
            self.observed_through = Some(next);
            self.metrics.observed_ticks = self.metrics.observed_ticks.saturating_add(1);
            let Some(value) = next.0.checked_add(1) else {
                break;
            };
            next = SimTick(value);
        }
        Ok(())
    }
}

impl RollbackEventDiscard for PresentationEventCursor {
    fn discard_after(&mut self, retained_through: SimTick) {
        if self
            .observed_through
            .is_some_and(|observed| observed > retained_through)
        {
            self.observed_through = Some(retained_through);
            self.metrics.rollback_rewinds = self.metrics.rollback_rewinds.saturating_add(1);
        }
    }
}

/// Rollback-safe policy router between canonical events and client presentation.
///
/// Already-consumed deduplicated IDs intentionally survive rollback. The event
/// journal and pending confirmed queue are rewound, but replaying a corrected
/// tick must not play the same impact sound or spawn the same particle twice.
#[derive(Resource, Clone, Debug, Default)]
pub struct PresentationEventRouter {
    presented: PresentedEventHistory,
    pending_confirmed: ConfirmedEventQueue,
    confirmed_through: Option<SimTick>,
    metrics: PresentationEventMetrics,
}

impl PresentationEventRouter {
    pub const fn confirmed_through(&self) -> Option<SimTick> {
        self.confirmed_through
    }

    pub const fn metrics(&self) -> PresentationEventMetrics {
        self.metrics
    }

    pub const fn pending_confirmed_len(&self) -> usize {
        self.pending_confirmed.len()
    }

    /// Observes one event produced by the current predicted timeline. The
    /// callback runs immediately only when the event's policy permits it.
    pub fn observe_predicted(
        &mut self,
        event: SimEvent,
        mut dispatch: impl FnMut(SimEvent),
    ) -> Result<(), EventEmitError> {
        match event.kind.presentation_policy() {
            PresentationPolicy::Predicted => {
                dispatch(event);
                self.metrics.predicted_dispatched =
                    self.metrics.predicted_dispatched.saturating_add(1);
            }
            PresentationPolicy::PredictedDeduplicated => {
                if self.presented.mark_if_new(event.id) {
                    dispatch(event);
                    self.metrics.deduplicated_dispatched =
                        self.metrics.deduplicated_dispatched.saturating_add(1);
                } else {
                    self.metrics.duplicate_events_suppressed =
                        self.metrics.duplicate_events_suppressed.saturating_add(1);
                }
            }
            PresentationPolicy::ConfirmedOnly => {
                if self
                    .confirmed_through
                    .is_some_and(|tick| event.id.tick <= tick)
                {
                    if self.presented.mark_if_new(event.id) {
                        dispatch(event);
                        self.metrics.confirmed_events_dispatched =
                            self.metrics.confirmed_events_dispatched.saturating_add(1);
                    } else {
                        self.metrics.duplicate_events_suppressed =
                            self.metrics.duplicate_events_suppressed.saturating_add(1);
                    }
                } else {
                    match self.pending_confirmed.push_if_new(event) {
                        Ok(true) => {
                            self.metrics.confirmed_events_queued =
                                self.metrics.confirmed_events_queued.saturating_add(1);
                        }
                        Ok(false) => {
                            self.metrics.duplicate_events_suppressed =
                                self.metrics.duplicate_events_suppressed.saturating_add(1);
                        }
                        Err(error) => {
                            self.metrics.confirmed_queue_overflows =
                                self.metrics.confirmed_queue_overflows.saturating_add(1);
                            return Err(error);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Advances the authority-confirmed frontier and dispatches newly safe
    /// irreversible events in canonical queue order. Regressions are ignored.
    pub fn confirm_through(&mut self, tick: SimTick, mut dispatch: impl FnMut(SimEvent)) {
        if self.confirmed_through.is_some_and(|current| tick < current) {
            return;
        }
        self.confirmed_through = Some(tick);
        let presented = &mut self.presented;
        let metrics = &mut self.metrics;
        self.pending_confirmed.drain_confirmed(tick, |event| {
            if presented.mark_if_new(event.id) {
                dispatch(event);
                metrics.confirmed_events_dispatched =
                    metrics.confirmed_events_dispatched.saturating_add(1);
            } else {
                metrics.duplicate_events_suppressed =
                    metrics.duplicate_events_suppressed.saturating_add(1);
            }
        });
    }

    /// Rewinds speculative canonical work. Presented IDs are deliberately not
    /// removed: resimulation must not replay one-shot presentation.
    pub fn discard_unconfirmed_after(&mut self, tick: SimTick) {
        let before = self.pending_confirmed.len();
        self.pending_confirmed.discard_after(tick);
        self.metrics.rollback_queue_discards = self
            .metrics
            .rollback_queue_discards
            .saturating_add((before - self.pending_confirmed.len()) as u64);
    }
}

impl RollbackEventDiscard for PresentationEventRouter {
    fn discard_after(&mut self, retained_through: SimTick) {
        self.discard_unconfirmed_after(retained_through);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fighter(value: u8) -> FighterId {
        FighterId::new(value).unwrap()
    }

    fn event_at(tick: u64, ordinal: u16) -> SimEvent {
        SimEvent {
            id: SimEventId {
                tick: SimTick(tick),
                source: SimEventSource::Fighter(fighter(0)),
                ordinal,
            },
            kind: SimEventKind::StockLost {
                fighter: fighter(0),
                stocks_remaining: 2,
            },
        }
    }

    fn commit_event(journal: &mut SimEventJournal, event: SimEvent) {
        let mut buffer = TickEventBuffer::new(event.id.tick);
        let emitted = buffer.emit(event.id.source, event.kind).unwrap();
        assert_eq!(emitted, event.id);
        journal.commit(&buffer);
    }

    #[test]
    fn presentation_cursor_routes_all_ticks_between_render_frames() {
        let mut journal = SimEventJournal::default();
        for tick in 40..44 {
            commit_event(&mut journal, event_at(tick, 0));
        }
        let mut router = PresentationEventRouter::default();
        let mut cursor = PresentationEventCursor::default();
        let mut dispatched = Vec::new();

        cursor
            .route_available(&journal, &mut router, Some(SimTick(43)), |event| {
                dispatched.push(event.id)
            })
            .unwrap();

        assert_eq!(
            dispatched,
            (40..44)
                .map(|tick| event_at(tick, 0).id)
                .collect::<Vec<_>>()
        );
        assert_eq!(cursor.observed_through(), Some(SimTick(43)));
        assert_eq!(cursor.metrics().observed_ticks, 4);
        assert_eq!(cursor.metrics().observed_events, 4);
    }

    #[test]
    fn presentation_cursor_waits_for_authority_confirmation() {
        let mut journal = SimEventJournal::default();
        commit_event(&mut journal, event_at(9, 0));
        let mut router = PresentationEventRouter::default();
        let mut cursor = PresentationEventCursor::default();
        let mut dispatched = Vec::new();

        cursor
            .route_available(&journal, &mut router, None, |event| {
                dispatched.push(event.id)
            })
            .unwrap();
        assert!(dispatched.is_empty());
        assert_eq!(router.pending_confirmed_len(), 1);

        cursor
            .route_available(&journal, &mut router, Some(SimTick(9)), |event| {
                dispatched.push(event.id)
            })
            .unwrap();
        assert_eq!(dispatched, vec![event_at(9, 0).id]);
        assert_eq!(router.pending_confirmed_len(), 0);
    }

    #[test]
    fn presentation_cursor_rewind_reobserves_but_deduplicates_one_shots() {
        let hit = SimEvent {
            id: SimEventId {
                tick: SimTick(12),
                source: SimEventSource::Fighter(fighter(0)),
                ordinal: 0,
            },
            kind: SimEventKind::HitConfirmed {
                attacker: Some(fighter(0)),
                victim: fighter(1),
                damage_q: 512,
                reaction: ReactionFamilyId::MediumStandingStagger,
            },
        };
        let mut journal = SimEventJournal::default();
        commit_event(&mut journal, hit);
        let mut router = PresentationEventRouter::default();
        let mut cursor = PresentationEventCursor::default();
        let mut dispatched = Vec::new();
        cursor
            .route_available(&journal, &mut router, None, |event| {
                dispatched.push(event.id)
            })
            .unwrap();

        cursor.discard_after(SimTick(11));
        router.discard_after(SimTick(11));
        journal.discard_after(SimTick(11));
        commit_event(&mut journal, hit);
        cursor
            .route_available(&journal, &mut router, None, |event| {
                dispatched.push(event.id)
            })
            .unwrap();

        assert_eq!(dispatched, vec![hit.id]);
        assert_eq!(cursor.metrics().rollback_rewinds, 1);
        assert_eq!(router.metrics().duplicate_events_suppressed, 1);
    }

    #[test]
    fn resimulation_produces_identical_event_ids() {
        let mut first = TickEventBuffer::new(SimTick(44));
        let mut second = TickEventBuffer::new(SimTick(44));
        for buffer in [&mut first, &mut second] {
            buffer
                .emit(
                    SimEventSource::Fighter(fighter(1)),
                    SimEventKind::HitConfirmed {
                        attacker: Some(fighter(1)),
                        victim: fighter(2),
                        damage_q: 4096,
                        reaction: ReactionFamilyId::GroundBounceDown,
                    },
                )
                .unwrap();
            buffer
                .emit(
                    SimEventSource::Fighter(fighter(2)),
                    SimEventKind::StockLost {
                        fighter: fighter(2),
                        stocks_remaining: 1,
                    },
                )
                .unwrap();
        }
        assert_eq!(
            first.iter().copied().collect::<Vec<_>>(),
            second.iter().copied().collect::<Vec<_>>()
        );
    }

    #[test]
    fn impact_events_support_neutral_authoritative_sources() {
        let guarded = SimEventKind::Guarded {
            attacker: None,
            defender: fighter(1),
        };
        let hit = SimEventKind::HitConfirmed {
            attacker: None,
            victim: fighter(2),
            damage_q: 4096,
            reaction: ReactionFamilyId::GroundedDownGetup,
        };

        assert!(matches!(
            guarded,
            SimEventKind::Guarded { attacker: None, .. }
        ));
        assert!(matches!(
            hit,
            SimEventKind::HitConfirmed { attacker: None, .. }
        ));
    }

    #[test]
    fn tick_buffer_rejects_newest_at_capacity_without_growing() {
        let mut buffer = TickEventBuffer::new(SimTick(9));
        for index in 0..MAX_SIM_EVENTS_PER_TICK {
            assert_eq!(
                buffer
                    .emit(
                        SimEventSource::Match,
                        SimEventKind::Statistic {
                            fighter: fighter(0),
                            statistic_id: index as u16,
                            delta: 1,
                        },
                    )
                    .unwrap()
                    .ordinal as usize,
                index
            );
        }
        assert_eq!(
            buffer.emit(
                SimEventSource::Match,
                SimEventKind::FighterRespawned {
                    fighter: fighter(0)
                }
            ),
            Err(EventEmitError::CapacityExceeded {
                capacity: MAX_SIM_EVENTS_PER_TICK
            })
        );
        assert_eq!(buffer.len(), MAX_SIM_EVENTS_PER_TICK);
        assert_eq!(buffer.overflow_count(), 1);
    }

    #[test]
    fn dedup_history_suppresses_rollback_replay_without_forgetting_consumed_ids() {
        let id = event_at(20, 0).id;
        let mut history = PresentedEventHistory::default();
        assert!(history.mark_if_new(id));
        assert!(!history.mark_if_new(id));
        assert!(!history.mark_if_new(id));
    }

    #[test]
    fn dedup_history_distinguishes_corrected_source_at_same_tick_and_ordinal() {
        let original = SimEventId {
            tick: SimTick(21),
            source: SimEventSource::Fighter(fighter(0)),
            ordinal: 7,
        };
        let corrected = SimEventId {
            source: SimEventSource::Fighter(fighter(1)),
            ..original
        };
        let mut history = PresentedEventHistory::default();

        assert!(history.mark_if_new(original));
        assert!(history.mark_if_new(corrected));
        assert!(history.contains(original));
        assert!(history.contains(corrected));
        assert!(!history.mark_if_new(original));
        assert!(!history.mark_if_new(corrected));
        assert_eq!(history.len(), 2);
    }

    #[test]
    fn dedup_history_covers_full_journal_and_evicts_only_modulo_tick() {
        let mut history = PresentedEventHistory::default();
        for tick in 0..SIM_EVENT_HISTORY_TICKS {
            for ordinal in 0..MAX_SIM_EVENTS_PER_TICK {
                assert!(history.mark_if_new(event_at(tick as u64, ordinal as u16).id));
            }
        }
        assert_eq!(history.len(), PRESENTED_EVENT_HISTORY);
        assert!(history.contains(event_at(0, 0).id));
        assert!(
            history.contains(
                event_at(
                    SIM_EVENT_HISTORY_TICKS as u64 - 1,
                    MAX_SIM_EVENTS_PER_TICK as u16 - 1,
                )
                .id
            )
        );

        for ordinal in 0..MAX_SIM_EVENTS_PER_TICK {
            assert!(
                history.mark_if_new(event_at(SIM_EVENT_HISTORY_TICKS as u64, ordinal as u16).id,)
            );
        }
        assert_eq!(history.len(), PRESENTED_EVENT_HISTORY);
        assert!(!history.contains(event_at(0, 0).id));
        assert!(
            history.contains(
                event_at(
                    SIM_EVENT_HISTORY_TICKS as u64,
                    MAX_SIM_EVENTS_PER_TICK as u16 - 1,
                )
                .id
            )
        );
    }

    #[test]
    fn dedup_history_retains_a_full_twelve_tick_rollback_storm() {
        let mut history = PresentedEventHistory::default();
        for tick in 40..52 {
            for ordinal in 0..MAX_SIM_EVENTS_PER_TICK {
                let id = event_at(tick, ordinal as u16).id;
                assert!(history.mark_if_new(id));
            }
        }
        assert_eq!(history.len(), 12 * MAX_SIM_EVENTS_PER_TICK);
        for tick in 40..52 {
            for ordinal in 0..MAX_SIM_EVENTS_PER_TICK {
                assert!(!history.mark_if_new(event_at(tick, ordinal as u16).id));
            }
        }
    }

    #[test]
    fn confirmed_queue_has_a_fixed_full_journal_upper_bound() {
        let mut queue = ConfirmedEventQueue::default();
        for tick in 0..SIM_EVENT_HISTORY_TICKS {
            for ordinal in 0..MAX_SIM_EVENTS_PER_TICK {
                queue.push(event_at(tick as u64, ordinal as u16)).unwrap();
            }
        }
        assert_eq!(queue.len(), MAX_PENDING_CONFIRMED_EVENTS);
        assert_eq!(
            queue.push(event_at(SIM_EVENT_HISTORY_TICKS as u64, 0)),
            Err(EventEmitError::CapacityExceeded {
                capacity: MAX_PENDING_CONFIRMED_EVENTS,
            })
        );
        assert_eq!(queue.dropped(), 1);
    }

    #[test]
    fn confirmed_queue_preserves_order_and_retains_future() {
        let mut queue = ConfirmedEventQueue::default();
        queue.push(event_at(8, 0)).unwrap();
        queue.push(event_at(10, 0)).unwrap();
        queue.push(event_at(10, 1)).unwrap();
        queue.push(event_at(12, 0)).unwrap();
        let mut consumed = Vec::new();
        queue.drain_confirmed(SimTick(10), |event| consumed.push(event.id));
        assert_eq!(
            consumed,
            vec![event_at(8, 0).id, event_at(10, 0).id, event_at(10, 1).id]
        );
        assert_eq!(queue.len(), 1);
        queue.drain_confirmed(SimTick(12), |event| consumed.push(event.id));
        assert_eq!(consumed.last(), Some(&event_at(12, 0).id));
        assert_eq!(queue.len(), 0);
    }

    #[test]
    fn presentation_router_never_replays_deduplicated_events_after_rollback() {
        let event = SimEvent {
            id: event_at(20, 0).id,
            kind: SimEventKind::HitConfirmed {
                attacker: Some(fighter(0)),
                victim: fighter(1),
                damage_q: 800,
                reaction: ReactionFamilyId::MediumStandingStagger,
            },
        };
        let mut router = PresentationEventRouter::default();
        let mut dispatched = Vec::new();
        router
            .observe_predicted(event, |event| dispatched.push(event.id))
            .unwrap();
        router.discard_unconfirmed_after(SimTick(19));
        router
            .observe_predicted(event, |event| dispatched.push(event.id))
            .unwrap();

        assert_eq!(dispatched, vec![event.id]);
        assert_eq!(router.metrics().duplicate_events_suppressed, 1);
    }

    #[test]
    fn presentation_router_dispatches_corrected_full_id_with_reused_ordinal() {
        let original = SimEvent {
            id: SimEventId {
                tick: SimTick(22),
                source: SimEventSource::Fighter(fighter(0)),
                ordinal: 3,
            },
            kind: SimEventKind::FighterRespawned {
                fighter: fighter(0),
            },
        };
        let corrected = SimEvent {
            id: SimEventId {
                source: SimEventSource::Fighter(fighter(1)),
                ..original.id
            },
            kind: SimEventKind::FighterRespawned {
                fighter: fighter(1),
            },
        };
        let mut router = PresentationEventRouter::default();
        let mut dispatched = Vec::new();

        router
            .observe_predicted(original, |event| dispatched.push(event.id))
            .unwrap();
        router.discard_unconfirmed_after(SimTick(21));
        router
            .observe_predicted(corrected, |event| dispatched.push(event.id))
            .unwrap();
        router
            .observe_predicted(original, |event| dispatched.push(event.id))
            .unwrap();

        assert_eq!(dispatched, vec![original.id, corrected.id]);
        assert_eq!(router.metrics().deduplicated_dispatched, 2);
        assert_eq!(router.metrics().duplicate_events_suppressed, 1);
    }

    #[test]
    fn presentation_router_holds_irreversible_events_until_confirmation() {
        let first = event_at(30, 0);
        let second = event_at(32, 0);
        let mut router = PresentationEventRouter::default();
        let mut dispatched = Vec::new();
        for event in [first, first, second] {
            router
                .observe_predicted(event, |event| dispatched.push(event.id))
                .unwrap();
        }
        assert!(dispatched.is_empty());
        assert_eq!(router.pending_confirmed_len(), 2);

        router.confirm_through(SimTick(30), |event| dispatched.push(event.id));
        assert_eq!(dispatched, vec![first.id]);
        assert_eq!(router.pending_confirmed_len(), 1);
        router.confirm_through(SimTick(32), |event| dispatched.push(event.id));
        assert_eq!(dispatched, vec![first.id, second.id]);
        assert_eq!(router.pending_confirmed_len(), 0);
    }

    #[test]
    fn rollback_drops_only_pending_confirmed_future_events() {
        let retained = event_at(40, 0);
        let discarded = event_at(42, 0);
        let mut router = PresentationEventRouter::default();
        router.observe_predicted(retained, |_| {}).unwrap();
        router.observe_predicted(discarded, |_| {}).unwrap();
        router.discard_unconfirmed_after(SimTick(40));
        assert_eq!(router.pending_confirmed_len(), 1);
        assert_eq!(router.metrics().rollback_queue_discards, 1);

        let mut dispatched = Vec::new();
        router.confirm_through(SimTick(42), |event| dispatched.push(event.id));
        assert_eq!(dispatched, vec![retained.id]);
    }

    #[test]
    fn event_policies_keep_results_and_progression_confirmed_only() {
        assert_eq!(
            SimEventKind::MatchResult {
                winner: Some(fighter(3)),
                result_id: 4,
            }
            .presentation_policy(),
            PresentationPolicy::ConfirmedOnly
        );
        assert_eq!(
            SimEventKind::MatchLifecycle {
                event: MatchLifecycleEvent::TimeUp,
            }
            .presentation_policy(),
            PresentationPolicy::PredictedDeduplicated
        );
        assert_eq!(
            SimEventKind::MatchLifecycle {
                event: MatchLifecycleEvent::Results,
            }
            .presentation_policy(),
            PresentationPolicy::ConfirmedOnly
        );
        assert_eq!(
            SimEventKind::ActionStarted {
                fighter: fighter(0),
                action_id: 2,
            }
            .presentation_policy(),
            PresentationPolicy::Predicted
        );
    }

    #[test]
    fn event_journal_is_tick_addressed_bounded_and_evicts_exact_modulo_slot() {
        let mut journal = SimEventJournal::default();
        for tick in 1..=SIM_EVENT_HISTORY_TICKS as u64 + 1 {
            let mut buffer = TickEventBuffer::new(SimTick(tick));
            buffer
                .emit(
                    SimEventSource::Fighter(fighter(0)),
                    SimEventKind::ActionStarted {
                        fighter: fighter(0),
                        action_id: tick as u16,
                    },
                )
                .unwrap();
            journal.commit(&buffer);
        }

        assert_eq!(journal.len(), SIM_EVENT_HISTORY_TICKS);
        assert_eq!(journal.oldest_tick(), Some(SimTick(2)));
        assert_eq!(
            journal.newest_tick(),
            Some(SimTick(SIM_EVENT_HISTORY_TICKS as u64 + 1))
        );
        assert!(journal.events_at(SimTick(1)).is_none());
        assert_eq!(journal.iter_at(SimTick(2)).len(), 1);
        assert_eq!(journal.metrics().overwritten_ticks, 1);
    }

    #[test]
    fn event_journal_discard_and_resimulation_rebuild_identical_ids() {
        let mut journal = SimEventJournal::default();
        let mut original = Vec::new();
        for tick in 20..=23 {
            let mut buffer = TickEventBuffer::new(SimTick(tick));
            let id = buffer
                .emit(
                    SimEventSource::Arena,
                    SimEventKind::EntityDespawned {
                        entity: SimEntityId::new(
                            crate::determinism::SimEntityKind::Hitbox,
                            tick as u32,
                            1,
                        ),
                    },
                )
                .unwrap();
            original.push(id);
            journal.commit(&buffer);
        }

        journal.discard_after(SimTick(21));
        assert_eq!(journal.newest_tick(), Some(SimTick(21)));
        assert!(journal.events_at(SimTick(22)).is_none());
        assert_eq!(journal.metrics().discarded_ticks, 2);

        for tick in 22..=23 {
            let mut buffer = TickEventBuffer::new(SimTick(tick));
            let id = buffer
                .emit(
                    SimEventSource::Arena,
                    SimEventKind::EntityDespawned {
                        entity: SimEntityId::new(
                            crate::determinism::SimEntityKind::Hitbox,
                            tick as u32,
                            1,
                        ),
                    },
                )
                .unwrap();
            assert_eq!(id, original[(tick - 20) as usize]);
            journal.commit(&buffer);
        }
        assert_eq!(journal.newest_tick(), Some(SimTick(23)));
    }

    #[test]
    fn beginning_a_new_tick_logically_clears_without_reusing_stale_events() {
        let mut buffer = TickEventBuffer::new(SimTick(1));
        buffer
            .emit(
                SimEventSource::Match,
                SimEventKind::FighterRespawned {
                    fighter: fighter(1),
                },
            )
            .unwrap();
        buffer.begin_tick(SimTick(2));
        assert!(buffer.is_empty());
        assert_eq!(buffer.iter().count(), 0);
        let id = buffer
            .emit(
                SimEventSource::Match,
                SimEventKind::FighterRespawned {
                    fighter: fighter(2),
                },
            )
            .unwrap();
        assert_eq!(id.tick, SimTick(2));
        assert_eq!(id.ordinal, 0);
    }
}
