//! Deterministic fixed-schedule harness for input tapes.
//!
//! This module drives Bevy's real `RunFixedMainLoop` with caller-provided render
//! deltas. Tape events are indexed by canonical simulation tick, so a 30 Hz
//! render frame that executes two fixed steps feeds the same inputs as two 60 Hz
//! render frames or four 120 Hz render frames. The harness deliberately keeps a
//! separate presentation clock: changing its scale never changes `Time<Virtual>`
//! or the number of 60 Hz simulation steps.
//!
//! The probe state is intentionally small. Future gameplay fixtures can retain
//! the tape/runner and replace [`DeterministicProbeState`] with canonical game
//! snapshots and hashes.

use std::time::Duration;

use bevy::prelude::*;
use bevy::time::{Fixed, Real, TimePlugin, TimeUpdateStrategy};

use crate::simulation::{SIM_HZ, SimTick, advance_sim_tick};
use crate::tick_input::{
    LOCAL_SEAT_COUNT, LocalSeatId, LocalTickInputState, RenderInputSample, TickInputFrame,
};

/// A render-sampled input transition assigned to one canonical simulation step.
///
/// `SimTick(0)` is the pre-step initial state, so playable tape events begin at
/// tick one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StepInputEvent {
    pub tick: u64,
    pub seat: LocalSeatId,
    pub sample: RenderInputSample,
}

impl StepInputEvent {
    pub const fn new(tick: u64, seat: LocalSeatId, sample: RenderInputSample) -> Self {
        Self { tick, seat, sample }
    }
}

/// Reusable, step-indexed input events.
///
/// Construction performs a stable sort by tick. Events targeting the same tick
/// retain authoring order, which matters because held state and movement use the
/// latest render sample while transition masks are union-latched.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FixedInputTape {
    events: Vec<StepInputEvent>,
}

impl FixedInputTape {
    pub fn new(mut events: Vec<StepInputEvent>) -> Self {
        assert!(
            events.iter().all(|event| event.tick > 0),
            "fixed input tape events must target tick one or later"
        );
        events.sort_by_key(|event| event.tick);
        Self { events }
    }

    pub fn events(&self) -> &[StepInputEvent] {
        &self.events
    }
}

#[derive(Resource, Clone, Debug)]
struct TapePlayback {
    tape: FixedInputTape,
    cursor: usize,
}

impl TapePlayback {
    fn new(tape: FixedInputTape) -> Self {
        Self { tape, cursor: 0 }
    }

    fn remaining(&self) -> usize {
        self.tape.events.len().saturating_sub(self.cursor)
    }
}

/// A deterministic stand-in for future canonical gameplay state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeterministicProbeState {
    pub tick: u64,
    pub accumulated_x: [i64; LOCAL_SEAT_COUNT],
    pub accumulated_y: [i64; LOCAL_SEAT_COUNT],
    pub held: [u16; LOCAL_SEAT_COUNT],
    pub transition_score: [u64; LOCAL_SEAT_COUNT],
}

impl DeterministicProbeState {
    fn step(&mut self, frames: &[TickInputFrame; LOCAL_SEAT_COUNT]) {
        self.tick = frames[0].tick;
        for (seat_index, frame) in frames.iter().enumerate() {
            debug_assert_eq!(frame.tick, self.tick);
            debug_assert_eq!(frame.seat.index(), seat_index);
            self.accumulated_x[seat_index] =
                self.accumulated_x[seat_index].wrapping_add(frame.movement.x as i64);
            self.accumulated_y[seat_index] =
                self.accumulated_y[seat_index].wrapping_add(frame.movement.y as i64);
            self.held[seat_index] = frame.held.bits();

            let pressed = frame.pressed.bits() as u64;
            let released = frame.released.bits() as u64;
            let sequence = frame.sequence.value() as u64;
            self.transition_score[seat_index] = self.transition_score[seat_index]
                .rotate_left(7)
                .wrapping_add(pressed.wrapping_mul(0x9e37))
                .wrapping_add(released.wrapping_mul(0x85eb))
                .wrapping_add(sequence);
        }
    }

    /// Stable FNV-1a hash over the explicit canonical field representation.
    pub fn canonical_hash(self) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        hash_bytes(&mut hash, &self.tick.to_le_bytes());
        for value in self.accumulated_x {
            hash_bytes(&mut hash, &value.to_le_bytes());
        }
        for value in self.accumulated_y {
            hash_bytes(&mut hash, &value.to_le_bytes());
        }
        for value in self.held {
            hash_bytes(&mut hash, &value.to_le_bytes());
        }
        for value in self.transition_score {
            hash_bytes(&mut hash, &value.to_le_bytes());
        }
        hash
    }
}

fn hash_bytes(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= *byte as u64;
        *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

/// One canonical fixed-step observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HarnessTickRecord {
    pub tick: u64,
    pub frames: [TickInputFrame; LOCAL_SEAT_COUNT],
    pub probe: DeterministicProbeState,
    pub hash: u64,
}

#[derive(Resource, Clone, Debug, Default, PartialEq, Eq)]
struct HarnessTrace(Vec<HarnessTickRecord>);

/// Client-only clock used to prove that visual time scaling is not simulation
/// time scaling.
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
struct PresentationProbeClock {
    scale: f64,
    elapsed_seconds: f64,
}

impl PresentationProbeClock {
    fn new(scale: f64) -> Self {
        assert!(scale.is_finite() && scale >= 0.0);
        Self {
            scale,
            elapsed_seconds: 0.0,
        }
    }
}

/// Complete result of running a tape through explicit render frames.
#[derive(Clone, Debug, PartialEq)]
pub struct HarnessRun {
    pub trace: Vec<HarnessTickRecord>,
    pub fixed_steps: u64,
    pub render_elapsed: Duration,
    pub presentation_elapsed_seconds: f64,
    pub unconsumed_events: usize,
}

impl HarnessRun {
    pub fn final_probe(&self) -> Option<DeterministicProbeState> {
        self.trace.last().map(|record| record.probe)
    }

    pub fn final_hash(&self) -> Option<u64> {
        self.trace.last().map(|record| record.hash)
    }
}

/// Runs a deterministic tape through Bevy's real fixed-main loop.
///
/// `render_deltas` are applied one at a time through
/// [`TimeUpdateStrategy::ManualDuration`]. Their sum therefore determines the
/// number of fixed steps; the tape never reads wall-clock time.
pub fn run_fixed_input_tape(
    tape: FixedInputTape,
    render_deltas: &[Duration],
    presentation_scale: f64,
) -> HarnessRun {
    let mut app = build_harness_app(tape, presentation_scale);
    prime_manual_time(&mut app);
    for delta in render_deltas {
        advance_render_frame(&mut app, *delta);
    }

    let trace = app.world().resource::<HarnessTrace>().0.clone();
    let fixed_steps = app.world().resource::<SimTick>().get();
    let presentation_elapsed_seconds = app
        .world()
        .resource::<PresentationProbeClock>()
        .elapsed_seconds;
    let unconsumed_events = app.world().resource::<TapePlayback>().remaining();
    HarnessRun {
        trace,
        fixed_steps,
        render_elapsed: render_deltas.iter().copied().sum(),
        presentation_elapsed_seconds,
        unconsumed_events,
    }
}

fn build_harness_app(tape: FixedInputTape, presentation_scale: f64) -> App {
    let mut app = App::new();
    app.add_plugins(TimePlugin)
        .insert_resource(Time::<Fixed>::from_hz(SIM_HZ))
        .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::ZERO))
        .init_resource::<SimTick>()
        .init_resource::<LocalTickInputState>()
        .insert_resource(TapePlayback::new(tape))
        .init_resource::<DeterministicProbeResource>()
        .init_resource::<HarnessTrace>()
        .insert_resource(PresentationProbeClock::new(presentation_scale))
        .add_systems(
            FixedUpdate,
            (advance_sim_tick, feed_step_events, drain_and_step_probe).chain(),
        )
        .add_systems(Update, advance_presentation_probe);
    app
}

#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
struct DeterministicProbeResource(DeterministicProbeState);

fn feed_step_events(
    tick: Res<SimTick>,
    mut playback: ResMut<TapePlayback>,
    mut inputs: ResMut<LocalTickInputState>,
) {
    while let Some(event) = playback.tape.events.get(playback.cursor).copied() {
        if event.tick > tick.get() {
            break;
        }
        playback.cursor += 1;
        if event.tick == tick.get() {
            inputs.merge_render_sample(event.seat, event.sample);
        }
    }
}

fn drain_and_step_probe(
    tick: Res<SimTick>,
    mut inputs: ResMut<LocalTickInputState>,
    mut probe: ResMut<DeterministicProbeResource>,
    mut trace: ResMut<HarnessTrace>,
) {
    let frames = std::array::from_fn(|seat_index| {
        let seat = LocalSeatId::new(seat_index).expect("harness seat is valid");
        inputs.drain_for_tick(seat, tick.get())
    });
    probe.0.step(&frames);
    let canonical_probe = probe.0;
    trace.0.push(HarnessTickRecord {
        tick: tick.get(),
        frames,
        probe: canonical_probe,
        hash: canonical_probe.canonical_hash(),
    });
}

fn advance_presentation_probe(
    real_time: Res<Time<Real>>,
    mut presentation: ResMut<PresentationProbeClock>,
) {
    presentation.elapsed_seconds += real_time.delta().as_secs_f64() * presentation.scale;
}

fn prime_manual_time(app: &mut App) {
    *app.world_mut().resource_mut::<TimeUpdateStrategy>() =
        TimeUpdateStrategy::ManualDuration(Duration::ZERO);
    app.update();
}

fn advance_render_frame(app: &mut App, delta: Duration) {
    *app.world_mut().resource_mut::<TimeUpdateStrategy>() =
        TimeUpdateStrategy::ManualDuration(delta);
    app.update();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tick_input::{InputMask, QuantizedMovement};

    fn seat(index: usize) -> LocalSeatId {
        LocalSeatId::new(index).expect("test seat is valid")
    }

    fn sample(
        movement: QuantizedMovement,
        held: InputMask,
        pressed: InputMask,
        released: InputMask,
    ) -> RenderInputSample {
        RenderInputSample {
            movement,
            held,
            pressed,
            released,
        }
    }

    fn representative_tape() -> FixedInputTape {
        FixedInputTape::new(vec![
            StepInputEvent::new(
                1,
                seat(0),
                sample(
                    QuantizedMovement::new(127, 0),
                    InputMask::RIGHT,
                    InputMask::RIGHT,
                    InputMask::NONE,
                ),
            ),
            StepInputEvent::new(
                3,
                seat(0),
                sample(
                    QuantizedMovement::ZERO,
                    InputMask::NONE,
                    InputMask::NONE,
                    InputMask::RIGHT,
                ),
            ),
            // A complete between-tick tap carries both transitions.
            StepInputEvent::new(
                4,
                seat(0),
                sample(
                    QuantizedMovement::ZERO,
                    InputMask::NONE,
                    InputMask::JUMP,
                    InputMask::JUMP,
                ),
            ),
            StepInputEvent::new(
                10,
                seat(0),
                sample(
                    QuantizedMovement::ZERO,
                    InputMask::HEAVY,
                    InputMask::HEAVY,
                    InputMask::NONE,
                ),
            ),
            StepInputEvent::new(
                15,
                seat(0),
                sample(
                    QuantizedMovement::ZERO,
                    InputMask::NONE,
                    InputMask::NONE,
                    InputMask::HEAVY,
                ),
            ),
            StepInputEvent::new(
                20,
                seat(1),
                sample(
                    QuantizedMovement::new(0, -127),
                    InputMask::UP | InputMask::LIGHT,
                    InputMask::UP | InputMask::LIGHT,
                    InputMask::NONE,
                ),
            ),
            // Same-step samples retain order: held/movement use this later one,
            // while both press masks reach tick 30.
            StepInputEvent::new(
                30,
                seat(2),
                sample(
                    QuantizedMovement::ZERO,
                    InputMask::LIGHT,
                    InputMask::LIGHT,
                    InputMask::NONE,
                ),
            ),
            StepInputEvent::new(
                30,
                seat(2),
                sample(
                    QuantizedMovement::new(90, 90),
                    InputMask::LIGHT | InputMask::HEAVY,
                    InputMask::HEAVY,
                    InputMask::NONE,
                ),
            ),
            StepInputEvent::new(
                45,
                seat(3),
                sample(
                    QuantizedMovement::new(-127, 0),
                    InputMask::LEFT | InputMask::AIM_GRAB,
                    InputMask::LEFT | InputMask::AIM_GRAB,
                    InputMask::NONE,
                ),
            ),
            StepInputEvent::new(
                75,
                seat(1),
                sample(
                    QuantizedMovement::ZERO,
                    InputMask::NONE,
                    InputMask::NONE,
                    InputMask::UP | InputMask::LIGHT,
                ),
            ),
        ])
    }

    fn fixed_timestep() -> Duration {
        Time::<Fixed>::from_hz(SIM_HZ).timestep()
    }

    fn deltas_30_hz(fixed_steps: u32) -> Vec<Duration> {
        let fixed = fixed_timestep();
        let mut deltas = vec![fixed * 2; (fixed_steps / 2) as usize];
        if fixed_steps % 2 != 0 {
            deltas.push(fixed);
        }
        deltas
    }

    fn deltas_60_hz(fixed_steps: u32) -> Vec<Duration> {
        vec![fixed_timestep(); fixed_steps as usize]
    }

    fn deltas_120_hz(fixed_steps: u32) -> Vec<Duration> {
        let fixed = fixed_timestep();
        let first_half = fixed / 2;
        let second_half = fixed - first_half;
        (0..fixed_steps)
            .flat_map(|_| [first_half, second_half])
            .collect()
    }

    fn irregular_deltas(fixed_steps: u32) -> Vec<Duration> {
        let mut remaining = fixed_timestep() * fixed_steps;
        let pattern = [
            Duration::from_millis(2),
            Duration::from_millis(41),
            Duration::from_millis(7),
            Duration::from_millis(3),
            Duration::from_millis(29),
            Duration::from_millis(11),
            Duration::from_millis(50),
            Duration::from_millis(5),
        ];
        let mut deltas = Vec::new();
        let mut index = 0;
        while remaining > Duration::ZERO {
            let delta = pattern[index % pattern.len()].min(remaining);
            deltas.push(delta);
            remaining -= delta;
            index += 1;
        }
        deltas
    }

    #[test]
    fn step_tape_is_identical_at_30_60_120_and_irregular_render_rates() {
        let fixed_steps = 120;
        let tape = representative_tape();
        let baseline = run_fixed_input_tape(tape.clone(), &deltas_60_hz(fixed_steps), 1.0);
        assert_eq!(baseline.fixed_steps, fixed_steps as u64);
        assert_eq!(baseline.trace.len(), fixed_steps as usize);
        assert_eq!(baseline.unconsumed_events, 0);

        for deltas in [
            deltas_30_hz(fixed_steps),
            deltas_120_hz(fixed_steps),
            irregular_deltas(fixed_steps),
        ] {
            let run = run_fixed_input_tape(tape.clone(), &deltas, 1.0);
            assert_eq!(run.fixed_steps, fixed_steps as u64);
            assert_eq!(run.trace, baseline.trace);
            assert_eq!(run.final_probe(), baseline.final_probe());
            assert_eq!(run.final_hash(), baseline.final_hash());
            assert_eq!(run.unconsumed_events, 0);
        }
    }

    #[test]
    fn real_fixed_main_loop_preserves_edges_across_zero_then_multiple_steps() {
        let fixed = fixed_timestep();
        let first_half = fixed / 2;
        let second_half = fixed - first_half;
        let mut app = build_harness_app(FixedInputTape::default(), 1.0);
        prime_manual_time(&mut app);

        app.world_mut()
            .resource_mut::<LocalTickInputState>()
            .merge_render_sample(
                seat(0),
                sample(
                    QuantizedMovement::ZERO,
                    InputMask::JUMP,
                    InputMask::JUMP,
                    InputMask::NONE,
                ),
            );
        advance_render_frame(&mut app, first_half);
        assert_eq!(app.world().resource::<SimTick>().get(), 0);
        assert!(app.world().resource::<HarnessTrace>().0.is_empty());

        advance_render_frame(&mut app, second_half);
        assert_eq!(app.world().resource::<SimTick>().get(), 1);
        let first = app.world().resource::<HarnessTrace>().0[0];
        assert_eq!(first.frames[0].held, InputMask::JUMP);
        assert_eq!(first.frames[0].pressed, InputMask::JUMP);
        assert_eq!(first.frames[0].released, InputMask::NONE);

        app.world_mut()
            .resource_mut::<LocalTickInputState>()
            .merge_render_sample(
                seat(0),
                sample(
                    QuantizedMovement::ZERO,
                    InputMask::NONE,
                    InputMask::NONE,
                    InputMask::JUMP,
                ),
            );
        // One render frame catches up three fixed ticks.
        advance_render_frame(&mut app, fixed * 3);
        assert_eq!(app.world().resource::<SimTick>().get(), 4);
        let trace = &app.world().resource::<HarnessTrace>().0;
        assert_eq!(trace[1].frames[0].released, InputMask::JUMP);
        assert_eq!(trace[1].frames[0].held, InputMask::NONE);
        for record in &trace[2..=3] {
            assert_eq!(record.frames[0].held, InputMask::NONE);
            assert_eq!(record.frames[0].pressed, InputMask::NONE);
            assert_eq!(record.frames[0].released, InputMask::NONE);
        }
        assert_eq!(trace[1].frames[0].sequence.value(), 1);
        assert_eq!(trace[3].frames[0].sequence.value(), 3);
    }

    #[test]
    fn presentation_scale_never_changes_manual_delta_simulation_ticks() {
        let fixed_steps = 90;
        let deltas = irregular_deltas(fixed_steps);
        let tape = representative_tape();
        let frozen_presentation = run_fixed_input_tape(tape.clone(), &deltas, 0.0);
        let accelerated_presentation = run_fixed_input_tape(tape, &deltas, 3.0);

        assert_eq!(frozen_presentation.fixed_steps, fixed_steps as u64);
        assert_eq!(accelerated_presentation.fixed_steps, fixed_steps as u64);
        assert_eq!(frozen_presentation.trace, accelerated_presentation.trace);
        assert_eq!(frozen_presentation.presentation_elapsed_seconds, 0.0);
        let expected = accelerated_presentation.render_elapsed.as_secs_f64() * 3.0;
        assert!(
            (accelerated_presentation.presentation_elapsed_seconds - expected).abs() < 0.000_001
        );
    }

    #[test]
    fn stable_sort_preserves_same_tick_authoring_order() {
        let early_tick = StepInputEvent::new(1, seat(0), RenderInputSample::default());
        let first_same_tick = StepInputEvent::new(
            2,
            seat(0),
            sample(
                QuantizedMovement::new(1, 0),
                InputMask::LIGHT,
                InputMask::LIGHT,
                InputMask::NONE,
            ),
        );
        let second_same_tick = StepInputEvent::new(
            2,
            seat(0),
            sample(
                QuantizedMovement::new(2, 0),
                InputMask::HEAVY,
                InputMask::HEAVY,
                InputMask::LIGHT,
            ),
        );
        let tape = FixedInputTape::new(vec![second_same_tick, early_tick, first_same_tick]);
        assert_eq!(
            tape.events(),
            &[early_tick, second_same_tick, first_same_tick]
        );

        // Stable sorting preserves the relative order supplied by the author;
        // it does not invent an order based on seat or sample contents.
        let run = run_fixed_input_tape(tape, &deltas_60_hz(2), 1.0);
        assert_eq!(
            run.trace[1].frames[0].movement,
            QuantizedMovement::new(1, 0)
        );
        assert_eq!(
            run.trace[1].frames[0].pressed,
            InputMask::LIGHT | InputMask::HEAVY
        );
        assert_eq!(run.trace[1].frames[0].released, InputMask::LIGHT);
    }
}
