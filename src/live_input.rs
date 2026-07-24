//! Canonical bridge between render-sampled local controls, AFC wire input, and
//! the gameplay-facing [`FighterInput`](crate::components::FighterInput).
//!
//! Local gesture recognition runs in integer simulation ticks, then writes the
//! same bounded `InputFrame` representation accepted from remote peers and bots.
//! Delayed light/heavy action pulses and their original raw press edges have
//! distinct bits so guard-counter behavior survives the protocol boundary.

use bevy::prelude::Vec2;

use crate::components::FighterInput;
use crate::network_protocol::{
    InputButtons, InputFrame, InputSequence as NetworkInputSequence, QuantizedAxis, SeatId, SimTick,
};
use crate::tick_input::{InputMask, SeatGestureTrackers, TickInputFrame};

/// Converts one raw local tick frame into the action-level protocol frame used
/// by offline, listen, dedicated, replay, and predicted simulations.
pub fn local_tick_to_network_input(
    frame: TickInputFrame,
    gestures: &mut SeatGestureTrackers,
) -> InputFrame {
    let dash = gestures
        .dash
        .register_presses(frame.pressed & InputMask::DIRECTIONS, frame.tick);
    let chord = gestures
        .chord
        .resolve(frame.pressed, frame.held, frame.tick);

    let mut held = 0_u16;
    set(
        &mut held,
        InputButtons::AIM_GRAB,
        frame.held.contains(InputMask::AIM_GRAB),
    );
    set(
        &mut held,
        InputButtons::LIGHT,
        frame.held.contains(InputMask::LIGHT),
    );
    set(
        &mut held,
        InputButtons::HEAVY,
        frame.held.contains(InputMask::HEAVY),
    );
    set(&mut held, InputButtons::GUARD, chord.guard);

    let mut pressed = 0_u16;
    set(
        &mut pressed,
        InputButtons::JUMP,
        frame.pressed.contains(InputMask::JUMP),
    );
    set(&mut pressed, InputButtons::DASH, !dash.is_empty());
    set(&mut pressed, InputButtons::LIGHT, chord.light);
    set(&mut pressed, InputButtons::HEAVY, chord.heavy);
    set(&mut pressed, InputButtons::AIM_GRAB, chord.grab);
    set(&mut pressed, InputButtons::GUARD, chord.guard);
    set(&mut pressed, InputButtons::ULTIMATE, chord.ultimate);
    set(
        &mut pressed,
        InputButtons::RAW_LIGHT,
        frame.pressed.contains(InputMask::LIGHT),
    );
    set(
        &mut pressed,
        InputButtons::RAW_HEAVY,
        frame.pressed.contains(InputMask::HEAVY),
    );

    let mut released = 0_u16;
    set(
        &mut released,
        InputButtons::HEAVY,
        frame.released.contains(InputMask::HEAVY),
    );

    InputFrame {
        tick: SimTick(frame.tick),
        seat: SeatId::new(frame.seat.index() as u8)
            .expect("local seats are a subset of protocol seats"),
        movement_x: QuantizedAxis::new(frame.movement.x)
            .expect("local movement excludes the reserved -128 axis"),
        movement_y: QuantizedAxis::new(frame.movement.y)
            .expect("local movement excludes the reserved -128 axis"),
        held_buttons: InputButtons::new(held).expect("local held buttons are supported"),
        pressed_buttons: InputButtons::new(pressed).expect("local pressed buttons are supported"),
        released_buttons: InputButtons::new(released)
            .expect("local released buttons are supported"),
        sequence: NetworkInputSequence(frame.sequence.value()),
    }
}

/// Applies a validated action-level AFC input frame to the live gameplay
/// component. Callers own seat-to-fighter authorization before this boundary.
pub fn network_input_to_fighter_input(frame: InputFrame) -> FighterInput {
    debug_assert!(frame.validate().is_ok());
    let held = frame.held_buttons.bits();
    let pressed = frame.pressed_buttons.bits();
    let released = frame.released_buttons.bits();
    // Preserve authored analog magnitude (bots use partial strafes) while
    // clamping diagonal device input to the gameplay unit circle.
    let raw_movement = Vec2::new(axis(frame.movement_x), axis(frame.movement_y));
    let movement = if crate::canonical_math::vec2_length_squared(raw_movement) > 1.0 {
        crate::canonical_math::vec2_normalize_or_zero(raw_movement)
    } else {
        raw_movement
    };

    FighterInput {
        movement,
        aim: has(held, InputButtons::AIM_GRAB),
        jump: has(pressed, InputButtons::JUMP),
        dash: has(pressed, InputButtons::DASH),
        light: has(pressed, InputButtons::LIGHT),
        light_held: has(held, InputButtons::LIGHT),
        raw_light_pressed: has(pressed, InputButtons::RAW_LIGHT),
        heavy: has(pressed, InputButtons::HEAVY),
        heavy_held: has(held, InputButtons::HEAVY),
        raw_heavy_pressed: has(pressed, InputButtons::RAW_HEAVY),
        heavy_released: has(released, InputButtons::HEAVY),
        grab: has(pressed, InputButtons::AIM_GRAB),
        guard: has(held | pressed, InputButtons::GUARD),
        ultimate: has(pressed, InputButtons::ULTIMATE),
        special: has(pressed, InputButtons::SPECIAL),
    }
}

/// Encodes an authority-generated gameplay input (currently bots) into the same
/// bounded protocol frame used by human peers. Device gesture recognition is
/// intentionally absent: the bot has already selected action-level booleans.
pub fn fighter_input_to_network_input(
    input: &FighterInput,
    tick: SimTick,
    seat: SeatId,
    sequence: NetworkInputSequence,
) -> InputFrame {
    let mut held = 0_u16;
    set(&mut held, InputButtons::AIM_GRAB, input.aim);
    set(&mut held, InputButtons::LIGHT, input.light_held);
    set(&mut held, InputButtons::HEAVY, input.heavy_held);
    set(&mut held, InputButtons::GUARD, input.guard);

    let mut pressed = 0_u16;
    set(&mut pressed, InputButtons::JUMP, input.jump);
    set(&mut pressed, InputButtons::DASH, input.dash);
    set(&mut pressed, InputButtons::LIGHT, input.light);
    set(&mut pressed, InputButtons::HEAVY, input.heavy);
    set(&mut pressed, InputButtons::AIM_GRAB, input.grab);
    set(&mut pressed, InputButtons::ULTIMATE, input.ultimate);
    set(&mut pressed, InputButtons::SPECIAL, input.special);
    set(
        &mut pressed,
        InputButtons::RAW_LIGHT,
        input.raw_light_pressed,
    );
    set(
        &mut pressed,
        InputButtons::RAW_HEAVY,
        input.raw_heavy_pressed,
    );

    let mut released = 0_u16;
    set(&mut released, InputButtons::HEAVY, input.heavy_released);

    InputFrame {
        tick,
        seat,
        movement_x: quantize_axis(input.movement.x),
        movement_y: quantize_axis(input.movement.y),
        held_buttons: InputButtons::new(held).expect("generated held buttons are supported"),
        pressed_buttons: InputButtons::new(pressed)
            .expect("generated pressed buttons are supported"),
        released_buttons: InputButtons::new(released)
            .expect("generated released buttons are supported"),
        sequence,
    }
}

fn axis(value: QuantizedAxis) -> f32 {
    f32::from(value.get()) / f32::from(QuantizedAxis::MAX)
}

fn quantize_axis(value: f32) -> QuantizedAxis {
    let quantized = (value.clamp(-1.0, 1.0) * f32::from(QuantizedAxis::MAX)).round() as i8;
    QuantizedAxis::new(quantized).expect("clamped generated axes exclude the reserved -128 value")
}

const fn set(bits: &mut u16, flag: u16, enabled: bool) {
    if enabled {
        *bits |= flag;
    }
}

const fn has(bits: u16, flag: u16) -> bool {
    bits & flag != 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tick_input::{
        InputSequence, LocalSeatId, LocalTickInputState, QuantizedMovement, RawInputButton,
        RenderInputSample,
    };

    fn local(
        tick: u64,
        held: InputMask,
        pressed: InputMask,
        released: InputMask,
    ) -> TickInputFrame {
        TickInputFrame {
            tick,
            seat: LocalSeatId::new(0).unwrap(),
            sequence: InputSequence(tick as u16),
            movement: QuantizedMovement::new(64, -32),
            held,
            pressed,
            released,
        }
    }

    #[test]
    fn raw_accumulator_release_turns_a_short_aim_hold_into_one_grab() {
        let seat = LocalSeatId::new(0).unwrap();
        let mut accumulator = LocalTickInputState::default();
        accumulator.merge_render_sample(
            seat,
            RenderInputSample {
                held: InputMask::AIM_GRAB,
                pressed: InputMask::AIM_GRAB,
                ..RenderInputSample::default()
            },
        );
        let pressed_raw = accumulator.drain_for_tick(seat, 10);
        let pressed_action =
            local_tick_to_network_input(pressed_raw, accumulator.gestures_mut(seat));
        assert!(has(
            pressed_action.held_buttons.bits(),
            InputButtons::AIM_GRAB
        ));
        assert!(!has(
            pressed_action.pressed_buttons.bits(),
            InputButtons::AIM_GRAB
        ));

        accumulator.merge_render_sample(
            seat,
            RenderInputSample {
                released: InputMask::AIM_GRAB,
                ..RenderInputSample::default()
            },
        );
        let released_raw = accumulator.drain_for_tick(seat, 11);
        assert!(released_raw.released.contains(InputMask::AIM_GRAB));
        let released_action =
            local_tick_to_network_input(released_raw, accumulator.gestures_mut(seat));
        assert!(!has(
            released_action.held_buttons.bits(),
            InputButtons::AIM_GRAB
        ));
        assert!(has(
            released_action.pressed_buttons.bits(),
            InputButtons::AIM_GRAB
        ));

        let next = local_tick_to_network_input(
            accumulator.drain_for_tick(seat, 12),
            accumulator.gestures_mut(seat),
        );
        assert!(!has(next.pressed_buttons.bits(), InputButtons::AIM_GRAB));
    }

    #[test]
    fn raw_accumulator_preserves_a_complete_aim_grab_tap_between_fixed_ticks() {
        let seat = LocalSeatId::new(0).unwrap();
        let mut accumulator = LocalTickInputState::default();
        accumulator.merge_render_sample(
            seat,
            RenderInputSample {
                held: InputMask::NONE,
                pressed: InputMask::AIM_GRAB,
                released: InputMask::AIM_GRAB,
                ..RenderInputSample::default()
            },
        );
        let raw = accumulator.drain_for_tick(seat, 25);
        assert!(raw.pressed.contains(InputMask::AIM_GRAB));
        assert!(raw.released.contains(InputMask::AIM_GRAB));
        assert!(!raw.held.contains(InputMask::AIM_GRAB));

        let action = local_tick_to_network_input(raw, accumulator.gestures_mut(seat));
        assert!(has(action.pressed_buttons.bits(), InputButtons::AIM_GRAB));
        assert!(!has(action.held_buttons.bits(), InputButtons::AIM_GRAB));
    }

    #[test]
    fn grab_release_stages_a_simultaneous_light_without_losing_raw_edge() {
        let mut gestures = SeatGestureTrackers::default();
        let _ = local_tick_to_network_input(
            local(
                30,
                InputMask::AIM_GRAB,
                InputMask::AIM_GRAB,
                InputMask::NONE,
            ),
            &mut gestures,
        );
        let release = local_tick_to_network_input(
            local(31, InputMask::LIGHT, InputMask::LIGHT, InputMask::AIM_GRAB),
            &mut gestures,
        );
        assert_eq!(
            release.pressed_buttons.bits(),
            InputButtons::AIM_GRAB | InputButtons::RAW_LIGHT
        );
        let delayed = local_tick_to_network_input(
            local(36, InputMask::LIGHT, InputMask::NONE, InputMask::NONE),
            &mut gestures,
        );
        assert_eq!(delayed.pressed_buttons.bits(), InputButtons::LIGHT);
    }

    #[test]
    fn complete_grab_tap_preserves_same_tick_and_expiring_attacks() {
        let mut same_tick = SeatGestureTrackers::default();
        let tap_with_heavy = local_tick_to_network_input(
            local(
                10,
                InputMask::NONE,
                InputMask::AIM_GRAB | InputMask::HEAVY,
                InputMask::AIM_GRAB | InputMask::HEAVY,
            ),
            &mut same_tick,
        );
        assert_eq!(
            tap_with_heavy.pressed_buttons.bits(),
            InputButtons::AIM_GRAB | InputButtons::RAW_HEAVY
        );
        let delayed_heavy = local_tick_to_network_input(
            local(15, InputMask::NONE, InputMask::NONE, InputMask::NONE),
            &mut same_tick,
        );
        assert_eq!(delayed_heavy.pressed_buttons.bits(), InputButtons::HEAVY);

        let mut expiring = SeatGestureTrackers::default();
        let _ = local_tick_to_network_input(
            local(20, InputMask::LIGHT, InputMask::LIGHT, InputMask::NONE),
            &mut expiring,
        );
        let coemitted = local_tick_to_network_input(
            local(
                25,
                InputMask::NONE,
                InputMask::AIM_GRAB,
                InputMask::AIM_GRAB,
            ),
            &mut expiring,
        );
        assert_eq!(
            coemitted.pressed_buttons.bits(),
            InputButtons::LIGHT | InputButtons::AIM_GRAB
        );
    }

    #[test]
    fn resetting_local_input_cancels_a_pending_grab_but_preserves_sequence() {
        let seat = LocalSeatId::new(0).unwrap();
        let mut accumulator = LocalTickInputState::default();
        accumulator.merge_render_sample(
            seat,
            RenderInputSample {
                held: InputMask::AIM_GRAB,
                pressed: InputMask::AIM_GRAB,
                ..RenderInputSample::default()
            },
        );
        let first = accumulator.drain_for_tick(seat, 1);
        let _ = local_tick_to_network_input(first, accumulator.gestures_mut(seat));

        accumulator.reset_seat_input(seat);
        let after_reset = accumulator.drain_for_tick(seat, 2);
        assert_eq!(after_reset.sequence, InputSequence(1));
        let action = local_tick_to_network_input(after_reset, accumulator.gestures_mut(seat));
        assert!(!has(action.pressed_buttons.bits(), InputButtons::AIM_GRAB));
    }

    #[test]
    fn delayed_solo_light_keeps_raw_edge_distinct_from_action_pulse() {
        let mut gestures = SeatGestureTrackers::default();
        let first = local_tick_to_network_input(
            local(10, InputMask::LIGHT, InputMask::LIGHT, InputMask::NONE),
            &mut gestures,
        );
        assert_eq!(
            first.pressed_buttons.bits(),
            InputButtons::RAW_LIGHT,
            "the chord grace tick carries only the raw edge"
        );
        let delayed = local_tick_to_network_input(
            local(15, InputMask::LIGHT, InputMask::NONE, InputMask::NONE),
            &mut gestures,
        );
        assert_eq!(delayed.pressed_buttons.bits(), InputButtons::LIGHT);

        let first_live = network_input_to_fighter_input(first);
        assert!(first_live.raw_light_pressed);
        assert!(!first_live.light);
        let delayed_live = network_input_to_fighter_input(delayed);
        assert!(!delayed_live.raw_light_pressed);
        assert!(delayed_live.light);
        assert!(delayed_live.light_held);
    }

    #[test]
    fn local_action_frame_round_trips_every_gameplay_input_class() {
        let mut gestures = SeatGestureTrackers::default();
        // Prime one directional tap, then complete the dash while pressing jump.
        let _ = local_tick_to_network_input(
            local(1, InputMask::RIGHT, InputMask::RIGHT, InputMask::NONE),
            &mut gestures,
        );
        let raw = local(
            2,
            InputMask::RIGHT | InputMask::AIM_GRAB | InputMask::HEAVY,
            InputMask::RIGHT | InputMask::HEAVY | InputMask::JUMP,
            InputMask::HEAVY,
        );
        let frame = local_tick_to_network_input(raw, &mut gestures);
        frame.validate().unwrap();
        assert!(has(frame.pressed_buttons.bits(), InputButtons::DASH));
        assert!(has(frame.pressed_buttons.bits(), InputButtons::JUMP));
        assert!(has(frame.pressed_buttons.bits(), InputButtons::RAW_HEAVY));
        assert!(has(frame.released_buttons.bits(), InputButtons::HEAVY));

        let live = network_input_to_fighter_input(frame);
        assert!(live.aim);
        assert!(live.jump);
        assert!(live.dash);
        assert!(live.heavy_held);
        assert!(live.raw_heavy_pressed);
        assert!(live.heavy_released);
        assert!(live.movement.length() <= 1.0);
        assert_eq!(raw.pressed.contains(RawInputButton::Jump.mask()), live.jump);
    }

    #[test]
    fn explicit_bot_or_remote_actions_map_without_local_gesture_state() {
        let frame = InputFrame {
            tick: SimTick(9),
            seat: SeatId::new(2).unwrap(),
            movement_x: QuantizedAxis::new(-127).unwrap(),
            movement_y: QuantizedAxis::new(127).unwrap(),
            held_buttons: InputButtons::new(
                InputButtons::AIM_GRAB | InputButtons::LIGHT | InputButtons::GUARD,
            )
            .unwrap(),
            pressed_buttons: InputButtons::new(
                InputButtons::AIM_GRAB
                    | InputButtons::LIGHT
                    | InputButtons::ULTIMATE
                    | InputButtons::SPECIAL,
            )
            .unwrap(),
            released_buttons: InputButtons::default(),
            sequence: NetworkInputSequence(44),
        };
        let live = network_input_to_fighter_input(frame);
        assert!(live.aim && live.grab && live.light && live.light_held);
        assert!(live.guard && live.ultimate && live.special);
        assert!((live.movement.length() - 1.0).abs() < 0.000_1);
    }

    #[test]
    fn authority_generated_input_round_trips_every_bot_action_bit() {
        let input = FighterInput {
            movement: Vec2::new(-0.5, 0.75),
            aim: true,
            jump: true,
            dash: true,
            light: true,
            light_held: true,
            raw_light_pressed: true,
            heavy: true,
            heavy_held: true,
            raw_heavy_pressed: true,
            heavy_released: true,
            grab: true,
            guard: true,
            ultimate: true,
            special: true,
        };
        let frame = fighter_input_to_network_input(
            &input,
            SimTick(55),
            SeatId::new(3).unwrap(),
            NetworkInputSequence(91),
        );
        frame.validate().unwrap();
        assert_eq!(frame.tick, SimTick(55));
        assert_eq!(frame.seat, SeatId::new(3).unwrap());
        assert_eq!(frame.sequence, NetworkInputSequence(91));

        let decoded = network_input_to_fighter_input(frame);
        assert!(decoded.aim && decoded.jump && decoded.dash);
        assert!(decoded.light && decoded.light_held && decoded.raw_light_pressed);
        assert!(decoded.heavy && decoded.heavy_held && decoded.raw_heavy_pressed);
        assert!(decoded.heavy_released && decoded.grab && decoded.guard);
        assert!(decoded.ultimate && decoded.special);
        assert!((decoded.movement.x + 0.5).abs() <= 1.0 / 127.0);
        assert!((decoded.movement.y - 0.75).abs() <= 1.0 / 127.0);
    }
}
