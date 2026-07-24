//! Fixed-step simulation clock and schedule ownership.
//!
//! The simulation tick is deliberately independent from match phase and
//! hitstop state. A client or authority advances it once for every execution of
//! the 60 Hz simulation schedule; match-relative clocks decide separately which
//! gameplay phases are allowed to advance.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

pub use crate::determinism::SimTick;

/// Canonical simulation frequency used by local play, replay, and networking.
pub const SIM_HZ: f64 = 60.0;
pub const SIM_HZ_U32: u32 = 60;
pub const SIM_DT_SECONDS: f32 = 1.0 / SIM_HZ_U32 as f32;

/// A rollback-safe countdown stored entirely in simulation ticks.
///
/// `u32::MAX` is reserved for an intentionally indefinite timer. Indefinite
/// timers never decrement, which replaces the legacy `f32::INFINITY` sentinel
/// without putting a non-finite value into canonical state.
#[repr(transparent)]
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct TickTimer(u32);

impl TickTimer {
    pub const ZERO: Self = Self(0);
    pub const INDEFINITE: Self = Self(u32::MAX);

    pub const fn from_ticks(ticks: u32) -> Self {
        Self(ticks)
    }

    pub fn from_seconds_ceil(seconds: f32) -> Self {
        if seconds == f32::INFINITY {
            Self::INDEFINITE
        } else {
            Self(seconds_to_ticks_ceil(seconds))
        }
    }

    pub const fn from_millis_ceil(milliseconds: u32) -> Self {
        Self(milliseconds_to_ticks_ceil(milliseconds))
    }

    pub const fn remaining(self) -> u32 {
        self.0
    }

    pub const fn active(self) -> bool {
        self.0 != 0
    }

    pub const fn is_indefinite(self) -> bool {
        self.0 == u32::MAX
    }

    pub fn clear(&mut self) {
        self.0 = 0;
    }

    pub fn set(&mut self, duration: Self) {
        *self = duration;
    }

    pub fn set_max(&mut self, duration: Self) {
        self.0 = self.0.max(duration.0);
    }

    /// Advances one simulation tick and returns true only on the active-to-zero
    /// transition. An indefinite timer is unchanged.
    pub fn tick(&mut self) -> bool {
        if self.0 == 0 || self.is_indefinite() {
            return false;
        }
        self.0 -= 1;
        self.0 == 0
    }

    /// Presentation/content compatibility view. Never use this value as stored
    /// authoritative state.
    pub fn as_seconds(self) -> f32 {
        if self.is_indefinite() {
            f32::INFINITY
        } else {
            self.0 as f32 / SIM_HZ_U32 as f32
        }
    }
}

/// An elapsed authoritative timeline. Systems increment it by exactly one for
/// each simulation step in which that timeline is allowed to advance.
#[repr(transparent)]
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct ElapsedTicks(u32);

impl ElapsedTicks {
    pub const ZERO: Self = Self(0);

    pub const fn from_ticks(ticks: u32) -> Self {
        Self(ticks)
    }

    pub const fn get(self) -> u32 {
        self.0
    }

    pub fn reset(&mut self) {
        self.0 = 0;
    }

    pub fn advance(&mut self) {
        self.0 = self.0.saturating_add(1);
    }

    pub fn as_seconds(self) -> f32 {
        self.0 as f32 / SIM_HZ_U32 as f32
    }

    /// Floors the rational tick time exactly, matching the legacy authored
    /// timeline's non-negative `elapsed_secs_to_ms` policy at 60 Hz.
    pub const fn as_millis_floor(self) -> u32 {
        ((self.0 as u64 * 1_000) / SIM_HZ_U32 as u64) as u32
    }
}

/// Converts authored milliseconds to the first 60 Hz tick at or after the
/// requested duration. Every positive duration occupies at least one tick.
pub const fn milliseconds_to_ticks_ceil(milliseconds: u32) -> u32 {
    if milliseconds == 0 {
        0
    } else {
        (((milliseconds as u64 * SIM_HZ_U32 as u64) + 999) / 1_000) as u32
    }
}

/// The global simulation tick currently being executed.
///
/// Tick zero represents the initial state before the first simulation step.
/// [`advance_sim_tick`] runs in [`SimulationSet::TickStart`], so systems in the
/// rest of the fixed schedule observe tick one on the first step. This clock is
/// monotonic across match resets and advances during hitstop.
impl Resource for SimTick {}

/// Ordered ownership boundaries for one canonical simulation step.
///
/// Configure these sets as one chain in `FixedUpdate`. Ordinary Bevy `chain`
/// ordering is intentional: Bevy 0.18 inserts deferred-command synchronization
/// on the ordering edges, preserving same-tick command visibility.
#[derive(SystemSet, Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum SimulationSet {
    TickStart,
    Match,
    Input,
    Action,
    Movement,
    Combat,
    Items,
    Respawn,
    TickEnd,
}

/// Selects who is allowed to advance the canonical simulation owned by a
/// rendered application.
///
/// The ordinary offline client leaves this at [`Self::Local`]. An online
/// client switches to [`Self::ExternalProjection`]: its predicted simulation
/// advances in a separate render-free world and the rendered world receives
/// snapshots/events through the presentation projection boundary. Keeping the
/// switch on the complete canonical set chain prevents a second, accidental
/// local authority from running behind the network client.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SimulationDriveMode {
    #[default]
    Local,
    ExternalProjection,
}

pub fn local_simulation_drive_enabled(mode: Option<Res<SimulationDriveMode>>) -> bool {
    mode.is_none_or(|mode| *mode == SimulationDriveMode::Local)
}

/// Advances the global clock once at the beginning of every fixed step.
///
/// Do not gate this system on match phase or hitstop. Network and input history
/// use this timeline even when selected gameplay phases are frozen.
pub fn advance_sim_tick(mut tick: ResMut<SimTick>) {
    tick.advance();
}

/// Converts an authored duration in seconds to canonical 60 Hz ticks.
///
/// Positive finite durations use ceiling conversion, ensuring an authored
/// active window never rounds down or disappears. Zero, negative values, and
/// NaN map to zero. Positive infinity or an overflowing finite duration
/// saturates at `u32::MAX` so the conversion remains deterministic; content
/// validation should reject those values before a match begins.
pub fn seconds_to_ticks_ceil(seconds: f32) -> u32 {
    if seconds.is_nan() || seconds <= 0.0 {
        return 0;
    }
    if seconds.is_infinite() {
        return u32::MAX;
    }

    let ticks = (seconds * SIM_HZ_U32 as f32).ceil();
    ticks.clamp(1.0, u32::MAX as f32) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Resource, Default)]
    struct ScheduleTrace(Vec<SimulationSet>);

    #[derive(Resource, Default)]
    struct TestHitstop(bool);

    #[derive(Resource, Default)]
    struct GameplaySteps(u32);

    fn trace_set(set: SimulationSet) -> impl FnMut(ResMut<ScheduleTrace>) + Clone {
        move |mut trace: ResMut<ScheduleTrace>| trace.0.push(set)
    }

    fn advance_unfrozen_gameplay(
        hitstop: Res<TestHitstop>,
        mut gameplay_steps: ResMut<GameplaySteps>,
    ) {
        if !hitstop.0 {
            gameplay_steps.0 += 1;
        }
    }

    fn configure_ordered_sets(app: &mut App) {
        app.configure_sets(
            FixedUpdate,
            (
                SimulationSet::TickStart,
                SimulationSet::Match,
                SimulationSet::Input,
                SimulationSet::Action,
                SimulationSet::Movement,
                SimulationSet::Combat,
                SimulationSet::Items,
                SimulationSet::Respawn,
                SimulationSet::TickEnd,
            )
                .chain(),
        );
    }

    fn configure_drive_gated_sets(app: &mut App) {
        app.configure_sets(
            FixedUpdate,
            (
                SimulationSet::TickStart,
                SimulationSet::Match,
                SimulationSet::Input,
                SimulationSet::Action,
                SimulationSet::Movement,
                SimulationSet::Combat,
                SimulationSet::Items,
                SimulationSet::Respawn,
                SimulationSet::TickEnd,
            )
                .chain()
                .run_if(local_simulation_drive_enabled),
        );
    }

    #[test]
    fn simulation_sets_execute_in_canonical_order() {
        let mut app = App::new();
        configure_ordered_sets(&mut app);
        app.init_resource::<ScheduleTrace>().add_systems(
            FixedUpdate,
            (
                trace_set(SimulationSet::TickStart).in_set(SimulationSet::TickStart),
                trace_set(SimulationSet::Match).in_set(SimulationSet::Match),
                trace_set(SimulationSet::Input).in_set(SimulationSet::Input),
                trace_set(SimulationSet::Action).in_set(SimulationSet::Action),
                trace_set(SimulationSet::Movement).in_set(SimulationSet::Movement),
                trace_set(SimulationSet::Combat).in_set(SimulationSet::Combat),
                trace_set(SimulationSet::Items).in_set(SimulationSet::Items),
                trace_set(SimulationSet::Respawn).in_set(SimulationSet::Respawn),
                trace_set(SimulationSet::TickEnd).in_set(SimulationSet::TickEnd),
            ),
        );

        app.world_mut().run_schedule(FixedUpdate);

        assert_eq!(
            app.world().resource::<ScheduleTrace>().0,
            vec![
                SimulationSet::TickStart,
                SimulationSet::Match,
                SimulationSet::Input,
                SimulationSet::Action,
                SimulationSet::Movement,
                SimulationSet::Combat,
                SimulationSet::Items,
                SimulationSet::Respawn,
                SimulationSet::TickEnd,
            ]
        );
    }

    #[test]
    fn global_tick_advances_while_gameplay_is_frozen_by_hitstop() {
        let mut app = App::new();
        configure_ordered_sets(&mut app);
        app.init_resource::<SimTick>()
            .insert_resource(TestHitstop(true))
            .init_resource::<GameplaySteps>()
            .add_systems(
                FixedUpdate,
                advance_sim_tick.in_set(SimulationSet::TickStart),
            )
            .add_systems(
                FixedUpdate,
                advance_unfrozen_gameplay.in_set(SimulationSet::Action),
            );

        for _ in 0..3 {
            app.world_mut().run_schedule(FixedUpdate);
        }

        assert_eq!(app.world().resource::<SimTick>().get(), 3);
        assert_eq!(app.world().resource::<GameplaySteps>().0, 0);

        app.world_mut().resource_mut::<TestHitstop>().0 = false;
        app.world_mut().run_schedule(FixedUpdate);

        assert_eq!(app.world().resource::<SimTick>().get(), 4);
        assert_eq!(app.world().resource::<GameplaySteps>().0, 1);
    }

    #[test]
    fn external_projection_disables_the_complete_local_simulation_chain() {
        let mut app = App::new();
        configure_drive_gated_sets(&mut app);
        app.insert_resource(SimulationDriveMode::ExternalProjection)
            .init_resource::<SimTick>()
            .init_resource::<GameplaySteps>()
            .add_systems(
                FixedUpdate,
                advance_sim_tick.in_set(SimulationSet::TickStart),
            )
            .add_systems(
                FixedUpdate,
                (|mut steps: ResMut<GameplaySteps>| steps.0 += 1).in_set(SimulationSet::Combat),
            );

        app.world_mut().run_schedule(FixedUpdate);
        assert_eq!(*app.world().resource::<SimTick>(), SimTick::ZERO);
        assert_eq!(app.world().resource::<GameplaySteps>().0, 0);

        app.world_mut().insert_resource(SimulationDriveMode::Local);
        app.world_mut().run_schedule(FixedUpdate);
        assert_eq!(*app.world().resource::<SimTick>(), SimTick(1));
        assert_eq!(app.world().resource::<GameplaySteps>().0, 1);
    }

    #[test]
    fn sim_tick_uses_explicit_wrapping_arithmetic() {
        let mut tick = SimTick(u64::MAX);
        tick.advance();
        assert_eq!(tick, SimTick(0));
    }

    #[test]
    fn duration_conversion_uses_ceiling_and_preserves_positive_windows() {
        assert_eq!(seconds_to_ticks_ceil(0.0), 0);
        assert_eq!(seconds_to_ticks_ceil(-0.25), 0);
        assert_eq!(seconds_to_ticks_ceil(f32::NAN), 0);
        assert_eq!(seconds_to_ticks_ceil(f32::INFINITY), u32::MAX);

        assert_eq!(seconds_to_ticks_ceil(0.000_001), 1);
        assert_eq!(seconds_to_ticks_ceil(1.0 / 60.0), 1);
        assert_eq!(seconds_to_ticks_ceil(0.08), 5);
        assert_eq!(seconds_to_ticks_ceil(0.1), 6);
        assert_eq!(seconds_to_ticks_ceil(0.28), 17);
        assert_eq!(seconds_to_ticks_ceil(1.0), 60);
    }

    #[test]
    fn millisecond_conversion_uses_the_same_ceiling_policy() {
        assert_eq!(milliseconds_to_ticks_ceil(0), 0);
        assert_eq!(milliseconds_to_ticks_ceil(1), 1);
        assert_eq!(milliseconds_to_ticks_ceil(16), 1);
        assert_eq!(milliseconds_to_ticks_ceil(17), 2);
        assert_eq!(milliseconds_to_ticks_ceil(100), 6);
        assert_eq!(milliseconds_to_ticks_ceil(280), 17);
        assert_eq!(milliseconds_to_ticks_ceil(1_000), 60);
        assert_eq!(milliseconds_to_ticks_ceil(u32::MAX), 257_698_038);
    }

    #[test]
    fn tick_timer_expires_once_and_indefinite_never_decrements() {
        let mut timer = TickTimer::from_ticks(2);
        assert!(timer.active());
        assert!(!timer.tick());
        assert_eq!(timer.remaining(), 1);
        assert!(timer.tick());
        assert!(!timer.active());
        assert!(!timer.tick());

        let mut indefinite = TickTimer::from_seconds_ceil(f32::INFINITY);
        assert!(indefinite.is_indefinite());
        assert!(!indefinite.tick());
        assert_eq!(indefinite, TickTimer::INDEFINITE);
    }

    #[test]
    fn elapsed_ticks_use_exact_rational_milliseconds() {
        let mut elapsed = ElapsedTicks::ZERO;
        for _ in 0..17 {
            elapsed.advance();
        }
        assert_eq!(elapsed.get(), 17);
        assert_eq!(elapsed.as_millis_floor(), 283);
        assert_eq!(ElapsedTicks::from_ticks(60).as_millis_floor(), 1_000);
        elapsed.reset();
        assert_eq!(elapsed, ElapsedTicks::ZERO);
    }
}
