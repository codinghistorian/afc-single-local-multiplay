//! Render-rate device input accumulation for a fixed-rate simulation.
//!
//! The intended schedule boundary is:
//!
//! 1. A `PreUpdate` system, ordered after Bevy's input systems, converts local
//!    device bindings into [`RenderInputSample`] values and merges them into
//!    [`LocalTickInputState`].
//! 2. The first input system in each `FixedUpdate` drains one [`TickInputFrame`]
//!    per active seat.
//!
//! Held state and movement use the newest render sample. Pressed and released
//! transitions are latched until a fixed tick consumes them. Consequently, a
//! render frame with no fixed step cannot lose a tap, while catch-up frames with
//! several fixed steps expose transitions only to the first step and repeat the
//! held state for the remaining steps.
//!
//! This module deliberately does not depend on `FighterInput`. Translating a
//! [`TickInputFrame`] and the gesture helpers below into the current gameplay
//! component is an integration step, and can later be replaced by the network
//! input-frame boundary without changing device sampling.

use std::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign};

use bevy::prelude::{Resource, Vec2};
use serde::{Deserialize, Serialize};

/// Animal Fighter Club currently supports at most four occupied seats.
pub const LOCAL_SEAT_COUNT: usize = 4;

/// The current 280 ms double-tap window, quantized upward at 60 Hz.
pub const DEFAULT_DASH_WINDOW_TICKS: u64 = 17;

/// The current 80 ms guard/ultimate chord grace, quantized upward at 60 Hz.
pub const DEFAULT_CHORD_GRACE_TICKS: u64 = 5;

/// A validated local seat index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LocalSeatId(u8);

impl LocalSeatId {
    pub const fn new(index: usize) -> Option<Self> {
        if index < LOCAL_SEAT_COUNT {
            Some(Self(index as u8))
        } else {
            None
        }
    }

    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// The eight raw, rebindable actions in each current keyboard control set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum RawInputButton {
    Left = 0,
    Right = 1,
    Up = 2,
    Down = 3,
    AimGrab = 4,
    Heavy = 5,
    Light = 6,
    Jump = 7,
}

impl RawInputButton {
    pub const ALL: [Self; 8] = [
        Self::Left,
        Self::Right,
        Self::Up,
        Self::Down,
        Self::AimGrab,
        Self::Heavy,
        Self::Light,
        Self::Jump,
    ];

    pub const fn mask(self) -> InputMask {
        InputMask(1_u16 << self as u8)
    }
}

/// A compact set of raw input buttons.
///
/// The storage leaves eight high bits available for future action-level inputs
/// without changing the frame layout.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct InputMask(u16);

impl InputMask {
    pub const NONE: Self = Self(0);
    pub const LEFT: Self = RawInputButton::Left.mask();
    pub const RIGHT: Self = RawInputButton::Right.mask();
    pub const UP: Self = RawInputButton::Up.mask();
    pub const DOWN: Self = RawInputButton::Down.mask();
    pub const AIM_GRAB: Self = RawInputButton::AimGrab.mask();
    pub const HEAVY: Self = RawInputButton::Heavy.mask();
    pub const LIGHT: Self = RawInputButton::Light.mask();
    pub const JUMP: Self = RawInputButton::Jump.mask();
    pub const DIRECTIONS: Self = Self(Self::LEFT.0 | Self::RIGHT.0 | Self::UP.0 | Self::DOWN.0);
    pub const CURRENT_BINDINGS: Self = Self((1_u16 << RawInputButton::ALL.len()) - 1);

    pub const fn from_bits(bits: u16) -> Self {
        Self(bits)
    }

    pub const fn bits(self) -> u16 {
        self.0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }

    pub fn remove(&mut self, other: Self) {
        self.0 &= !other.0;
    }
}

impl From<RawInputButton> for InputMask {
    fn from(button: RawInputButton) -> Self {
        button.mask()
    }
}

impl BitOr for InputMask {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for InputMask {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl BitAnd for InputMask {
    type Output = Self;

    fn bitand(self, rhs: Self) -> Self::Output {
        Self(self.0 & rhs.0)
    }
}

impl BitAndAssign for InputMask {
    fn bitand_assign(&mut self, rhs: Self) {
        self.0 &= rhs.0;
    }
}

/// Camera-relative movement quantized to two signed bytes.
///
/// Normal device sampling uses the symmetric `-127..=127` range. The `new`
/// constructor remains lossless so a future codec can detect and reject the
/// otherwise-unused `-128` wire value instead of silently rewriting it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct QuantizedMovement {
    pub x: i8,
    pub y: i8,
}

impl QuantizedMovement {
    pub const ZERO: Self = Self { x: 0, y: 0 };

    pub const fn new(x: i8, y: i8) -> Self {
        Self { x, y }
    }

    /// Quantizes already-normalized camera-relative axes.
    ///
    /// Non-finite input fails closed to zero on that axis. This method clamps
    /// but does not normalize diagonals; the local binding layer owns the same
    /// normalization policy used by gameplay today.
    pub fn from_unit_axes(x: f32, y: f32) -> Self {
        Self {
            x: quantize_axis(x),
            y: quantize_axis(y),
        }
    }

    pub fn to_unit_axes(self) -> [f32; 2] {
        [self.x as f32 / 127.0, self.y as f32 / 127.0]
    }
}

fn quantize_axis(value: f32) -> i8 {
    if !value.is_finite() {
        return 0;
    }
    (value.clamp(-1.0, 1.0) * 127.0).round() as i8
}

/// One effective input sample produced by a render-rate device-binding system.
///
/// `pressed` and `released` may both contain the same button. That represents a
/// complete tap between two simulation ticks and must not be sanitized away.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderInputSample {
    pub movement: QuantizedMovement,
    pub held: InputMask,
    pub pressed: InputMask,
    pub released: InputMask,
}

/// A per-seat sequence number. It advances once per produced simulation frame,
/// including frames that merely repeat continuous held state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct InputSequence(pub u16);

impl InputSequence {
    pub const fn value(self) -> u16 {
        self.0
    }

    pub const fn wrapping_next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }
}

/// The local action source consumed by one simulation tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TickInputFrame {
    pub tick: u64,
    pub seat: LocalSeatId,
    pub sequence: InputSequence,
    pub movement: QuantizedMovement,
    pub held: InputMask,
    pub pressed: InputMask,
    pub released: InputMask,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct RenderInputSourceState {
    movement: QuantizedMovement,
    held: InputMask,
}

impl RenderInputSourceState {
    fn update(&mut self, sample: RenderInputSample) {
        self.movement = sample.movement;
        self.held = sample.held;
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct SeatInputAccumulator {
    primary: RenderInputSourceState,
    steam_controller: RenderInputSourceState,
    pending_pressed: InputMask,
    pending_released: InputMask,
    next_sequence: InputSequence,
}

impl SeatInputAccumulator {
    fn merge(&mut self, sample: RenderInputSample) {
        self.primary.update(sample);
        let not_held_by_steam = InputMask::from_bits(!self.steam_controller.held.bits());
        self.pending_pressed |= sample.pressed & not_held_by_steam;
        self.pending_released |= sample.released & not_held_by_steam;
    }

    /// Steam Input exposes current action values rather than render-frame edge
    /// events. Keep that device source separate from keyboard input and derive
    /// transitions here so a controller cannot overwrite a simultaneously held
    /// keyboard action. The resulting edges retain the ordinary union-latched
    /// semantics until the next fixed tick drains them.
    fn merge_steam_controller(&mut self, movement: QuantizedMovement, held: InputMask) {
        let previous_effective = self.primary.held | self.steam_controller.held;
        self.steam_controller = RenderInputSourceState { movement, held };
        let next_effective = self.primary.held | self.steam_controller.held;
        self.pending_pressed |= next_effective & InputMask::from_bits(!previous_effective.bits());
        self.pending_released |= previous_effective & InputMask::from_bits(!next_effective.bits());
    }

    fn drain(&mut self, seat: LocalSeatId, tick: u64) -> TickInputFrame {
        let frame = TickInputFrame {
            tick,
            seat,
            sequence: self.next_sequence,
            movement: combine_movement(self.primary.movement, self.steam_controller.movement),
            held: self.primary.held | self.steam_controller.held,
            pressed: self.pending_pressed,
            released: self.pending_released,
        };
        self.pending_pressed = InputMask::NONE;
        self.pending_released = InputMask::NONE;
        self.next_sequence = self.next_sequence.wrapping_next();
        frame
    }

    /// Clears device state and transitions while keeping the connection/session
    /// sequence monotonic.
    fn reset_input_preserving_sequence(&mut self) {
        let next_sequence = self.next_sequence;
        *self = Self {
            next_sequence,
            ..Self::default()
        };
    }

    fn reset_all(&mut self) {
        *self = Self::default();
    }
}

fn combine_movement(
    primary: QuantizedMovement,
    steam_controller: QuantizedMovement,
) -> QuantizedMovement {
    let x = f32::from(primary.x) + f32::from(steam_controller.x);
    let y = f32::from(primary.y) + f32::from(steam_controller.y);
    let movement = Vec2::new(x, y);
    let maximum = 127.0_f32;
    let scale = if crate::canonical_math::vec2_length_squared(movement) > maximum * maximum {
        maximum / crate::canonical_math::vec2_length(movement)
    } else {
        1.0
    };
    QuantizedMovement::from_unit_axes(x * scale / 127.0, y * scale / 127.0)
}

/// Four-seat local input state shared across render and fixed schedules.
///
/// The dash and chord trackers live beside the accumulator because a seat/source
/// reset must clear all three atomically. They are deliberately not applied by
/// [`drain_for_tick`](Self::drain_for_tick); the fixed input integration chooses
/// when to interpret gestures and how to map the resulting action pulses.
#[derive(Resource, Clone, Debug, PartialEq, Eq)]
pub struct LocalTickInputState {
    seats: [SeatTickInputState; LOCAL_SEAT_COUNT],
}

impl Default for LocalTickInputState {
    fn default() -> Self {
        Self {
            seats: std::array::from_fn(|_| SeatTickInputState::default()),
        }
    }
}

impl LocalTickInputState {
    pub fn merge_render_sample(&mut self, seat: LocalSeatId, sample: RenderInputSample) {
        self.seats[seat.index()].accumulator.merge(sample);
    }

    /// Merges one current Steam Input action snapshot for a local ordinal.
    ///
    /// Passing zero movement and an empty held mask is also significant: it
    /// releases actions from a disconnected controller without disturbing the
    /// keyboard source or session sequence.
    pub fn merge_steam_controller_state(
        &mut self,
        seat: LocalSeatId,
        movement: QuantizedMovement,
        held: InputMask,
    ) {
        self.seats[seat.index()]
            .accumulator
            .merge_steam_controller(movement, held);
    }

    pub fn drain_for_tick(&mut self, seat: LocalSeatId, tick: u64) -> TickInputFrame {
        self.seats[seat.index()].accumulator.drain(seat, tick)
    }

    pub fn gestures_mut(&mut self, seat: LocalSeatId) -> &mut SeatGestureTrackers {
        &mut self.seats[seat.index()].gestures
    }

    pub fn next_sequence(&self, seat: LocalSeatId) -> InputSequence {
        self.seats[seat.index()].accumulator.next_sequence
    }

    pub fn set_next_sequence(&mut self, seat: LocalSeatId, sequence: InputSequence) {
        self.seats[seat.index()].accumulator.next_sequence = sequence;
    }

    /// Clears one seat after a phase, binding, or local source change while
    /// preserving its session-level frame sequence.
    pub fn reset_seat_input(&mut self, seat: LocalSeatId) {
        self.seats[seat.index()].reset_input_preserving_sequence();
    }

    /// Clears all seats after a phase or binding revision while preserving each
    /// session-level sequence.
    pub fn reset_all_input(&mut self) {
        for seat in &mut self.seats {
            seat.reset_input_preserving_sequence();
        }
    }

    /// Starts a fresh local input session, including sequence zero.
    pub fn reset_all_sessions(&mut self) {
        for seat in &mut self.seats {
            seat.reset_all();
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct SeatTickInputState {
    accumulator: SeatInputAccumulator,
    gestures: SeatGestureTrackers,
}

impl SeatTickInputState {
    fn reset_input_preserving_sequence(&mut self) {
        self.accumulator.reset_input_preserving_sequence();
        self.gestures.reset();
    }

    fn reset_all(&mut self) {
        self.accumulator.reset_all();
        self.gestures = SeatGestureTrackers::default();
    }
}

/// Raw directional buttons used by the double-tap recognizer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DashDirection {
    Left,
    Right,
    Down,
    Up,
}

impl DashDirection {
    const ORDERED: [Self; 4] = [Self::Left, Self::Right, Self::Down, Self::Up];

    const fn index(self) -> usize {
        match self {
            Self::Left => 0,
            Self::Right => 1,
            Self::Down => 2,
            Self::Up => 3,
        }
    }

    pub const fn mask(self) -> InputMask {
        match self {
            Self::Left => InputMask::LEFT,
            Self::Right => InputMask::RIGHT,
            Self::Down => InputMask::DOWN,
            Self::Up => InputMask::UP,
        }
    }
}

/// Tick-based directional double-tap recognition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DashTapTracker {
    last_taps: [Option<u64>; 4],
    window_ticks: u64,
}

impl Default for DashTapTracker {
    fn default() -> Self {
        Self::with_window(DEFAULT_DASH_WINDOW_TICKS)
    }
}

impl DashTapTracker {
    pub const fn with_window(window_ticks: u64) -> Self {
        Self {
            last_taps: [None; 4],
            window_ticks,
        }
    }

    pub const fn window_ticks(&self) -> u64 {
        self.window_ticks
    }

    /// Records every directional press in deterministic legacy priority order
    /// and returns the subset that completed a double tap this tick.
    pub fn register_presses(&mut self, pressed: InputMask, tick: u64) -> InputMask {
        let mut completed = InputMask::NONE;
        for direction in DashDirection::ORDERED {
            if pressed.contains(direction.mask()) && self.register_press(direction, tick) {
                completed.insert(direction.mask());
            }
        }
        completed
    }

    pub fn register_press(&mut self, direction: DashDirection, tick: u64) -> bool {
        let last_tap = &mut self.last_taps[direction.index()];
        let completed = last_tap.is_some_and(|last| tick.saturating_sub(last) <= self.window_ticks);
        *last_tap = Some(tick);
        completed
    }

    /// Clears tap history while preserving the configured window.
    pub fn reset(&mut self) {
        self.last_taps = [None; 4];
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChordButton {
    Light,
    Heavy,
    Grab,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ChordPending {
    button: ChordButton,
    started_tick: u64,
}

/// The action pulses/held guard state emitted by the local chord recognizer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChordOutput {
    pub light: bool,
    pub heavy: bool,
    pub grab: bool,
    pub guard: bool,
    pub ultimate: bool,
}

/// Tick-based version of the current light/heavy/aim guard and ultimate chord
/// recognizer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuardChordTracker {
    pending: Option<ChordPending>,
    guard_latched: bool,
    ultimate_latched: bool,
    light_pressed_tick: Option<u64>,
    heavy_pressed_tick: Option<u64>,
    grab_pressed_tick: Option<u64>,
    grace_ticks: u64,
}

impl Default for GuardChordTracker {
    fn default() -> Self {
        Self::with_grace(DEFAULT_CHORD_GRACE_TICKS)
    }
}

impl GuardChordTracker {
    pub const fn with_grace(grace_ticks: u64) -> Self {
        Self {
            pending: None,
            guard_latched: false,
            ultimate_latched: false,
            light_pressed_tick: None,
            heavy_pressed_tick: None,
            grab_pressed_tick: None,
            grace_ticks,
        }
    }

    pub const fn grace_ticks(&self) -> u64 {
        self.grace_ticks
    }

    /// Resolves one fixed tick. This must be called on every simulation tick,
    /// even when no render sample arrived, so pending solo attacks and the
    /// aim/grab tap window expire in simulation time rather than render time.
    ///
    /// `pressed` may contain `AIM_GRAB` while `held` does not. That is a
    /// complete press-and-release accumulated between fixed ticks and emits the
    /// grab pulse immediately. Otherwise an aim/grab press becomes a grab only
    /// when it is released no later than the inclusive grace boundary. Holding
    /// it through that boundary keeps aim active but cancels the pending grab.
    pub fn resolve(&mut self, pressed: InputMask, held: InputMask, tick: u64) -> ChordOutput {
        let light_just = pressed.contains(InputMask::LIGHT);
        let heavy_just = pressed.contains(InputMask::HEAVY);
        let grab_just = pressed.contains(InputMask::AIM_GRAB);
        let light_held = held.contains(InputMask::LIGHT);
        let heavy_held = held.contains(InputMask::HEAVY);
        let grab_held = held.contains(InputMask::AIM_GRAB);

        self.record_press_ticks(light_just, heavy_just, grab_just, tick);
        if self.ultimate_latched {
            if light_held && heavy_held && grab_held {
                return ChordOutput::default();
            }
            self.ultimate_latched = false;
        }

        if self.ultimate_chord_pressed(light_held, heavy_held, grab_held, tick) {
            return self.latch_ultimate();
        }

        if self.guard_latched {
            if light_held && heavy_held {
                return ChordOutput {
                    guard: true,
                    ..ChordOutput::default()
                };
            }
            self.guard_latched = false;
            self.pending = None;
            return ChordOutput::default();
        }

        // Ultimate was considered first. A simultaneous light/heavy edge is
        // therefore always guard and may never leak a pending grab pulse.
        if light_just && heavy_just {
            return self.latch_guard();
        }

        if let Some(pending) = self.pending {
            let elapsed = tick.saturating_sub(pending.started_tick);
            let opposite_arrived = match pending.button {
                ChordButton::Light => heavy_just && light_held,
                ChordButton::Heavy => light_just && heavy_held,
                ChordButton::Grab => false,
            };
            if opposite_arrived && elapsed <= self.grace_ticks {
                return self.latch_guard();
            }

            if pending.button == ChordButton::Grab {
                if !grab_held && elapsed <= self.grace_ticks {
                    // Preserve an attack edge that arrives on the grab-release
                    // tick. The grab pulse is emitted now; the attack retains
                    // the ordinary chord grace and resolves on a later tick.
                    self.pending = if light_just {
                        Some(ChordPending {
                            button: ChordButton::Light,
                            started_tick: tick,
                        })
                    } else if heavy_just {
                        Some(ChordPending {
                            button: ChordButton::Heavy,
                            started_tick: tick,
                        })
                    } else {
                        None
                    };
                    return ChordOutput {
                        grab: true,
                        ..ChordOutput::default()
                    };
                }
                if elapsed >= self.grace_ticks {
                    // Held at the inclusive boundary means aim-only. Once
                    // cancelled, a later release cannot synthesize a grab.
                    self.pending = if light_just {
                        Some(ChordPending {
                            button: ChordButton::Light,
                            started_tick: tick,
                        })
                    } else if heavy_just {
                        Some(ChordPending {
                            button: ChordButton::Heavy,
                            started_tick: tick,
                        })
                    } else {
                        None
                    };
                    return ChordOutput::default();
                }
                return ChordOutput::default();
            }

            let complete_grab_tap = grab_just && !grab_held;
            if complete_grab_tap {
                if elapsed < self.grace_ticks {
                    // A complete render-accumulated tap is never delayed. Keep
                    // the earlier solo attack pending under its original grace.
                    return ChordOutput {
                        grab: true,
                        ..ChordOutput::default()
                    };
                }
                self.pending = match (pending.button, light_just, heavy_just) {
                    (ChordButton::Light, _, true) => Some(ChordPending {
                        button: ChordButton::Heavy,
                        started_tick: tick,
                    }),
                    (ChordButton::Heavy, true, _) => Some(ChordPending {
                        button: ChordButton::Light,
                        started_tick: tick,
                    }),
                    _ => None,
                };
                return match pending.button {
                    ChordButton::Light => ChordOutput {
                        light: true,
                        grab: true,
                        ..ChordOutput::default()
                    },
                    ChordButton::Heavy => ChordOutput {
                        heavy: true,
                        grab: true,
                        ..ChordOutput::default()
                    },
                    ChordButton::Grab => unreachable!("grab pending returned above"),
                };
            }

            if elapsed >= self.grace_ticks {
                self.pending = match (pending.button, light_just, heavy_just, grab_just) {
                    (_, _, _, true) => Some(ChordPending {
                        button: ChordButton::Grab,
                        started_tick: tick,
                    }),
                    (ChordButton::Light, _, true, _) => Some(ChordPending {
                        button: ChordButton::Heavy,
                        started_tick: tick,
                    }),
                    (ChordButton::Heavy, true, _, _) => Some(ChordPending {
                        button: ChordButton::Light,
                        started_tick: tick,
                    }),
                    _ => None,
                };
                return match pending.button {
                    ChordButton::Light => ChordOutput {
                        light: true,
                        ..ChordOutput::default()
                    },
                    ChordButton::Heavy => ChordOutput {
                        heavy: true,
                        ..ChordOutput::default()
                    },
                    ChordButton::Grab => ChordOutput::default(),
                };
            }

            return ChordOutput::default();
        }

        if grab_just {
            if grab_held {
                self.pending = Some(ChordPending {
                    button: ChordButton::Grab,
                    started_tick: tick,
                });
            } else {
                self.pending = if light_just {
                    Some(ChordPending {
                        button: ChordButton::Light,
                        started_tick: tick,
                    })
                } else if heavy_just {
                    Some(ChordPending {
                        button: ChordButton::Heavy,
                        started_tick: tick,
                    })
                } else {
                    None
                };
                return ChordOutput {
                    grab: true,
                    ..ChordOutput::default()
                };
            }
        } else if light_just && heavy_just {
            return self.latch_guard();
        } else if light_just {
            self.pending = Some(ChordPending {
                button: ChordButton::Light,
                started_tick: tick,
            });
        } else if heavy_just {
            self.pending = Some(ChordPending {
                button: ChordButton::Heavy,
                started_tick: tick,
            });
        }

        ChordOutput::default()
    }

    /// Clears chord history while preserving the configured grace.
    pub fn reset(&mut self) {
        let grace_ticks = self.grace_ticks;
        *self = Self::with_grace(grace_ticks);
    }

    fn record_press_ticks(
        &mut self,
        light_just: bool,
        heavy_just: bool,
        grab_just: bool,
        tick: u64,
    ) {
        if light_just {
            self.light_pressed_tick = Some(tick);
        }
        if heavy_just {
            self.heavy_pressed_tick = Some(tick);
        }
        if grab_just {
            self.grab_pressed_tick = Some(tick);
        }
    }

    fn ultimate_chord_pressed(
        &self,
        light_held: bool,
        heavy_held: bool,
        grab_held: bool,
        tick: u64,
    ) -> bool {
        if !(light_held && heavy_held && grab_held) {
            return false;
        }
        let (Some(light), Some(heavy)) = (self.light_pressed_tick, self.heavy_pressed_tick) else {
            return false;
        };
        let latest_attack = light.max(heavy);
        if light.abs_diff(heavy) <= self.grace_ticks
            && tick.saturating_sub(latest_attack) <= self.grace_ticks
        {
            return true;
        }
        let Some(grab) = self.grab_pressed_tick else {
            return false;
        };
        let earliest = light.min(heavy).min(grab);
        let latest = light.max(heavy).max(grab);
        latest.saturating_sub(earliest) <= self.grace_ticks
    }

    fn latch_ultimate(&mut self) -> ChordOutput {
        self.pending = None;
        self.guard_latched = false;
        self.ultimate_latched = true;
        ChordOutput {
            ultimate: true,
            ..ChordOutput::default()
        }
    }

    fn latch_guard(&mut self) -> ChordOutput {
        self.pending = None;
        self.guard_latched = true;
        ChordOutput {
            guard: true,
            ..ChordOutput::default()
        }
    }
}

/// Local gesture history belonging to one seat.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SeatGestureTrackers {
    pub dash: DashTapTracker,
    pub chord: GuardChordTracker,
}

impl SeatGestureTrackers {
    pub fn reset(&mut self) {
        self.dash.reset();
        self.chord.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn seat_ids_reject_the_fifth_slot() {
        for index in 0..LOCAL_SEAT_COUNT {
            assert_eq!(seat(index).index(), index);
        }
        assert_eq!(LocalSeatId::new(LOCAL_SEAT_COUNT), None);
    }

    #[test]
    fn input_masks_support_composition_and_removal() {
        let mut mask = InputMask::LEFT | InputMask::LIGHT;
        assert!(mask.contains(InputMask::LEFT));
        assert!(mask.intersects(InputMask::DIRECTIONS));
        assert!(!mask.contains(InputMask::HEAVY));

        mask.insert(InputMask::HEAVY);
        assert!(mask.contains(InputMask::LIGHT | InputMask::HEAVY));
        mask.remove(InputMask::LEFT);
        assert!(!mask.intersects(InputMask::DIRECTIONS));
        assert_eq!(InputMask::CURRENT_BINDINGS.bits(), 0xff);
    }

    #[test]
    fn movement_quantization_is_symmetric_clamped_and_finite() {
        assert_eq!(
            QuantizedMovement::from_unit_axes(-1.0, 1.0),
            QuantizedMovement::new(-127, 127)
        );
        assert_eq!(
            QuantizedMovement::from_unit_axes(-2.0, 2.0),
            QuantizedMovement::new(-127, 127)
        );
        assert_eq!(
            QuantizedMovement::from_unit_axes(0.5, -0.5),
            QuantizedMovement::new(64, -64)
        );
        assert_eq!(
            QuantizedMovement::from_unit_axes(f32::NAN, f32::INFINITY),
            QuantizedMovement::ZERO
        );
        let decoded = QuantizedMovement::new(64, -64).to_unit_axes();
        assert!((decoded[0] - 64.0 / 127.0).abs() < f32::EPSILON);
        assert!((decoded[1] + 64.0 / 127.0).abs() < f32::EPSILON);
    }

    #[test]
    fn zero_fixed_steps_preserve_a_complete_between_tick_tap() {
        let mut state = LocalTickInputState::default();
        let player = seat(0);

        state.merge_render_sample(
            player,
            sample(
                QuantizedMovement::new(10, 0),
                InputMask::JUMP,
                InputMask::JUMP,
                InputMask::NONE,
            ),
        );
        // Another render sample arrives, but no fixed tick ran between them.
        state.merge_render_sample(
            player,
            sample(
                QuantizedMovement::new(20, 0),
                InputMask::NONE,
                InputMask::NONE,
                InputMask::JUMP,
            ),
        );

        let frame = state.drain_for_tick(player, 42);
        assert_eq!(frame.tick, 42);
        assert_eq!(frame.movement, QuantizedMovement::new(20, 0));
        assert_eq!(frame.held, InputMask::NONE);
        assert_eq!(frame.pressed, InputMask::JUMP);
        assert_eq!(frame.released, InputMask::JUMP);
    }

    #[test]
    fn multiple_fixed_steps_consume_edges_once_and_repeat_continuous_state() {
        let mut state = LocalTickInputState::default();
        let player = seat(0);
        let held = InputMask::RIGHT | InputMask::HEAVY;
        let movement = QuantizedMovement::new(127, 0);
        state.merge_render_sample(player, sample(movement, held, held, InputMask::NONE));

        let first = state.drain_for_tick(player, 100);
        let second = state.drain_for_tick(player, 101);
        let third = state.drain_for_tick(player, 102);

        assert_eq!(first.held, held);
        assert_eq!(first.pressed, held);
        assert_eq!(first.sequence, InputSequence(0));
        for (frame, sequence) in [(second, 1), (third, 2)] {
            assert_eq!(frame.movement, movement);
            assert_eq!(frame.held, held);
            assert_eq!(frame.pressed, InputMask::NONE);
            assert_eq!(frame.released, InputMask::NONE);
            assert_eq!(frame.sequence, InputSequence(sequence));
        }
    }

    #[test]
    fn transitions_from_several_render_samples_are_union_latched() {
        let mut state = LocalTickInputState::default();
        let player = seat(0);
        state.merge_render_sample(
            player,
            sample(
                QuantizedMovement::ZERO,
                InputMask::LIGHT,
                InputMask::LIGHT,
                InputMask::NONE,
            ),
        );
        state.merge_render_sample(
            player,
            sample(
                QuantizedMovement::ZERO,
                InputMask::HEAVY,
                InputMask::HEAVY,
                InputMask::LIGHT,
            ),
        );

        let frame = state.drain_for_tick(player, 9);
        assert_eq!(frame.held, InputMask::HEAVY);
        assert_eq!(frame.pressed, InputMask::LIGHT | InputMask::HEAVY);
        assert_eq!(frame.released, InputMask::LIGHT);
    }

    #[test]
    fn steam_controller_state_merges_with_keyboard_and_derives_edges() {
        let mut state = LocalTickInputState::default();
        let player = seat(0);
        state.merge_render_sample(
            player,
            sample(
                QuantizedMovement::new(127, 0),
                InputMask::RIGHT | InputMask::LIGHT,
                InputMask::LIGHT,
                InputMask::NONE,
            ),
        );
        state.merge_steam_controller_state(
            player,
            QuantizedMovement::new(-64, 127),
            InputMask::LEFT | InputMask::JUMP,
        );

        let first = state.drain_for_tick(player, 20);
        assert_eq!(first.movement, QuantizedMovement::new(56, 114));
        assert_eq!(
            first.held,
            InputMask::LEFT | InputMask::RIGHT | InputMask::LIGHT | InputMask::JUMP
        );
        assert_eq!(
            first.pressed,
            InputMask::LIGHT | InputMask::LEFT | InputMask::JUMP
        );

        // Releasing the controller leaves the keyboard continuous state intact
        // and latches only the controller-originated releases.
        state.merge_steam_controller_state(player, QuantizedMovement::ZERO, InputMask::NONE);
        let second = state.drain_for_tick(player, 21);
        assert_eq!(second.movement, QuantizedMovement::new(127, 0));
        assert_eq!(second.held, InputMask::RIGHT | InputMask::LIGHT);
        assert_eq!(second.pressed, InputMask::NONE);
        assert_eq!(second.released, InputMask::LEFT | InputMask::JUMP);
    }

    #[test]
    fn repeated_steam_controller_snapshot_does_not_repeat_pressed_edge() {
        let mut state = LocalTickInputState::default();
        let player = seat(3);
        state.merge_steam_controller_state(player, QuantizedMovement::ZERO, InputMask::HEAVY);
        assert_eq!(state.drain_for_tick(player, 1).pressed, InputMask::HEAVY);

        state.merge_steam_controller_state(player, QuantizedMovement::ZERO, InputMask::HEAVY);
        let held = state.drain_for_tick(player, 2);
        assert_eq!(held.held, InputMask::HEAVY);
        assert_eq!(held.pressed, InputMask::NONE);
        assert_eq!(held.released, InputMask::NONE);
    }

    #[test]
    fn releasing_one_device_does_not_release_an_action_held_by_the_other() {
        let mut state = LocalTickInputState::default();
        let player = seat(1);
        state.merge_render_sample(
            player,
            sample(
                QuantizedMovement::ZERO,
                InputMask::JUMP,
                InputMask::JUMP,
                InputMask::NONE,
            ),
        );
        state.merge_steam_controller_state(player, QuantizedMovement::ZERO, InputMask::JUMP);
        let first = state.drain_for_tick(player, 1);
        assert_eq!(first.pressed, InputMask::JUMP);

        state.merge_steam_controller_state(player, QuantizedMovement::ZERO, InputMask::NONE);
        let keyboard_still_held = state.drain_for_tick(player, 2);
        assert_eq!(keyboard_still_held.held, InputMask::JUMP);
        assert_eq!(keyboard_still_held.released, InputMask::NONE);

        state.merge_render_sample(
            player,
            sample(
                QuantizedMovement::ZERO,
                InputMask::NONE,
                InputMask::NONE,
                InputMask::JUMP,
            ),
        );
        let released = state.drain_for_tick(player, 3);
        assert_eq!(released.held, InputMask::NONE);
        assert_eq!(released.released, InputMask::JUMP);
    }

    #[test]
    fn four_seats_accumulate_drain_and_sequence_independently() {
        let mut state = LocalTickInputState::default();
        let buttons = [
            InputMask::LEFT,
            InputMask::RIGHT,
            InputMask::LIGHT,
            InputMask::HEAVY,
        ];
        for (index, button) in buttons.into_iter().enumerate() {
            state.merge_render_sample(
                seat(index),
                sample(
                    QuantizedMovement::new(index as i8, -(index as i8)),
                    button,
                    button,
                    InputMask::NONE,
                ),
            );
        }

        for (index, button) in buttons.into_iter().enumerate() {
            let frame = state.drain_for_tick(seat(index), 70);
            assert_eq!(frame.seat, seat(index));
            assert_eq!(frame.held, button);
            assert_eq!(frame.pressed, button);
            assert_eq!(frame.sequence, InputSequence(0));
            assert_eq!(state.next_sequence(seat(index)), InputSequence(1));
        }
    }

    #[test]
    fn sequence_wraps_without_affecting_input_state() {
        let mut state = LocalTickInputState::default();
        let player = seat(2);
        state.set_next_sequence(player, InputSequence(u16::MAX));
        state.merge_render_sample(
            player,
            sample(
                QuantizedMovement::ZERO,
                InputMask::AIM_GRAB,
                InputMask::NONE,
                InputMask::NONE,
            ),
        );

        let last = state.drain_for_tick(player, 1);
        let wrapped = state.drain_for_tick(player, 2);
        assert_eq!(last.sequence, InputSequence(u16::MAX));
        assert_eq!(wrapped.sequence, InputSequence(0));
        assert_eq!(wrapped.held, InputMask::AIM_GRAB);
    }

    #[test]
    fn source_reset_clears_input_and_gestures_but_preserves_sequence() {
        let mut state = LocalTickInputState::default();
        let player = seat(1);
        state.merge_render_sample(
            player,
            sample(
                QuantizedMovement::new(127, 0),
                InputMask::RIGHT,
                InputMask::RIGHT,
                InputMask::NONE,
            ),
        );
        state
            .gestures_mut(player)
            .dash
            .register_presses(InputMask::RIGHT, 10);
        state
            .gestures_mut(player)
            .chord
            .resolve(InputMask::LIGHT, InputMask::LIGHT, 10);
        let _ = state.drain_for_tick(player, 10);

        state.reset_seat_input(player);
        let cleared = state.drain_for_tick(player, 11);
        assert_eq!(cleared.sequence, InputSequence(1));
        assert_eq!(cleared.movement, QuantizedMovement::ZERO);
        assert_eq!(cleared.held, InputMask::NONE);
        assert_eq!(cleared.pressed, InputMask::NONE);
        assert!(
            state
                .gestures_mut(player)
                .dash
                .register_presses(InputMask::RIGHT, 11)
                .is_empty()
        );
        assert_eq!(
            state
                .gestures_mut(player)
                .chord
                .resolve(InputMask::NONE, InputMask::NONE, 15),
            ChordOutput::default()
        );
    }

    #[test]
    fn fresh_session_reset_restarts_sequences() {
        let mut state = LocalTickInputState::default();
        let player = seat(3);
        let _ = state.drain_for_tick(player, 1);
        assert_eq!(state.next_sequence(player), InputSequence(1));

        state.reset_all_sessions();
        assert_eq!(state.drain_for_tick(player, 2).sequence, InputSequence(0));
    }

    #[test]
    fn dash_window_accepts_tick_seventeen_and_rejects_tick_eighteen() {
        let mut tracker = DashTapTracker::default();
        assert!(tracker.register_presses(InputMask::RIGHT, 100).is_empty());
        assert_eq!(
            tracker.register_presses(InputMask::RIGHT, 117),
            InputMask::RIGHT
        );

        tracker.reset();
        assert!(tracker.register_presses(InputMask::RIGHT, 200).is_empty());
        assert!(tracker.register_presses(InputMask::RIGHT, 218).is_empty());
    }

    #[test]
    fn dash_directions_track_independently_and_all_presses_are_recorded() {
        let mut tracker = DashTapTracker::default();
        let both = InputMask::LEFT | InputMask::RIGHT;
        assert!(tracker.register_presses(both, 10).is_empty());
        assert_eq!(tracker.register_presses(both, 20), both);

        tracker.reset();
        assert!(tracker.register_presses(InputMask::LEFT, 30).is_empty());
        assert!(tracker.register_presses(InputMask::RIGHT, 31).is_empty());
        assert_eq!(
            tracker.register_presses(InputMask::LEFT, 47),
            InputMask::LEFT
        );
        assert!(tracker.register_presses(InputMask::RIGHT, 49).is_empty());
    }

    #[test]
    fn solo_attack_resolves_on_the_fifth_tick_without_new_render_input() {
        let mut tracker = GuardChordTracker::default();
        assert_eq!(
            tracker.resolve(InputMask::LIGHT, InputMask::LIGHT, 100),
            ChordOutput::default()
        );
        for tick in 101..105 {
            assert_eq!(
                tracker.resolve(InputMask::NONE, InputMask::LIGHT, tick),
                ChordOutput::default()
            );
        }
        assert_eq!(
            tracker.resolve(InputMask::NONE, InputMask::LIGHT, 105),
            ChordOutput {
                light: true,
                ..ChordOutput::default()
            }
        );
        assert_eq!(
            tracker.resolve(InputMask::NONE, InputMask::LIGHT, 106),
            ChordOutput::default()
        );
    }

    #[test]
    fn guard_chord_accepts_the_inclusive_grace_boundary_and_stays_held() {
        let mut tracker = GuardChordTracker::default();
        assert_eq!(
            tracker.resolve(InputMask::LIGHT, InputMask::LIGHT, 10),
            ChordOutput::default()
        );
        assert_eq!(
            tracker.resolve(InputMask::HEAVY, InputMask::LIGHT | InputMask::HEAVY, 15),
            ChordOutput {
                guard: true,
                ..ChordOutput::default()
            }
        );
        assert_eq!(
            tracker.resolve(InputMask::NONE, InputMask::LIGHT | InputMask::HEAVY, 16),
            ChordOutput {
                guard: true,
                ..ChordOutput::default()
            }
        );
        assert_eq!(
            tracker.resolve(InputMask::NONE, InputMask::HEAVY, 17),
            ChordOutput::default()
        );
    }

    #[test]
    fn late_second_guard_button_preserves_the_solo_attack() {
        let mut tracker = GuardChordTracker::default();
        assert_eq!(
            tracker.resolve(InputMask::LIGHT, InputMask::LIGHT, 20),
            ChordOutput::default()
        );
        assert_eq!(
            tracker.resolve(InputMask::HEAVY, InputMask::LIGHT | InputMask::HEAVY, 26),
            ChordOutput {
                light: true,
                ..ChordOutput::default()
            }
        );
        assert_eq!(
            tracker.resolve(InputMask::NONE, InputMask::LIGHT | InputMask::HEAVY, 31),
            ChordOutput {
                heavy: true,
                ..ChordOutput::default()
            }
        );
    }

    #[test]
    fn held_aim_plus_staggered_attack_buttons_emits_one_ultimate() {
        let mut tracker = GuardChordTracker::default();
        assert_eq!(
            tracker.resolve(InputMask::AIM_GRAB, InputMask::AIM_GRAB, 1),
            ChordOutput::default()
        );
        assert_eq!(
            tracker.resolve(InputMask::LIGHT, InputMask::AIM_GRAB | InputMask::LIGHT, 20),
            ChordOutput::default()
        );
        let all = InputMask::AIM_GRAB | InputMask::LIGHT | InputMask::HEAVY;
        assert_eq!(
            tracker.resolve(InputMask::HEAVY, all, 25),
            ChordOutput {
                ultimate: true,
                ..ChordOutput::default()
            }
        );
        assert_eq!(
            tracker.resolve(InputMask::NONE, all, 26),
            ChordOutput::default()
        );
        // Releasing any member unlatches it. Repeated held state after the
        // gesture window expires cannot invent another ultimate edge.
        assert_eq!(
            tracker.resolve(InputMask::NONE, InputMask::LIGHT | InputMask::HEAVY, 27),
            ChordOutput::default()
        );
        assert_eq!(
            tracker.resolve(InputMask::NONE, all, 31),
            ChordOutput::default()
        );
    }

    #[test]
    fn aim_grab_short_tap_emits_once_on_release() {
        let mut tracker = GuardChordTracker::default();
        assert_eq!(
            tracker.resolve(InputMask::AIM_GRAB, InputMask::AIM_GRAB, 40),
            ChordOutput::default()
        );
        assert_eq!(
            tracker.resolve(InputMask::NONE, InputMask::AIM_GRAB, 41),
            ChordOutput::default()
        );
        assert_eq!(
            tracker.resolve(InputMask::NONE, InputMask::NONE, 42),
            ChordOutput {
                grab: true,
                ..ChordOutput::default()
            }
        );
        assert_eq!(
            tracker.resolve(InputMask::NONE, InputMask::NONE, 43),
            ChordOutput::default()
        );
    }

    #[test]
    fn aim_grab_complete_same_tick_tap_emits_immediately() {
        let mut tracker = GuardChordTracker::default();
        assert_eq!(
            tracker.resolve(InputMask::AIM_GRAB, InputMask::NONE, 50),
            ChordOutput {
                grab: true,
                ..ChordOutput::default()
            }
        );
        assert_eq!(
            tracker.resolve(InputMask::NONE, InputMask::NONE, 51),
            ChordOutput::default()
        );
    }

    #[test]
    fn complete_grab_tap_stages_a_same_tick_solo_attack() {
        let mut light = GuardChordTracker::default();
        assert_eq!(
            light.resolve(InputMask::AIM_GRAB | InputMask::LIGHT, InputMask::NONE, 10,),
            ChordOutput {
                grab: true,
                ..ChordOutput::default()
            }
        );
        assert_eq!(
            light.resolve(InputMask::NONE, InputMask::NONE, 15),
            ChordOutput {
                light: true,
                ..ChordOutput::default()
            }
        );

        let mut heavy = GuardChordTracker::default();
        assert_eq!(
            heavy.resolve(InputMask::AIM_GRAB | InputMask::HEAVY, InputMask::NONE, 20,),
            ChordOutput {
                grab: true,
                ..ChordOutput::default()
            }
        );
        assert_eq!(
            heavy.resolve(InputMask::NONE, InputMask::NONE, 25),
            ChordOutput {
                heavy: true,
                ..ChordOutput::default()
            }
        );
    }

    #[test]
    fn complete_grab_tap_coemits_with_an_expiring_solo_attack() {
        let mut light = GuardChordTracker::default();
        assert_eq!(
            light.resolve(InputMask::LIGHT, InputMask::LIGHT, 30),
            ChordOutput::default()
        );
        assert_eq!(
            light.resolve(InputMask::AIM_GRAB, InputMask::NONE, 35),
            ChordOutput {
                light: true,
                grab: true,
                ..ChordOutput::default()
            }
        );

        let mut heavy = GuardChordTracker::default();
        assert_eq!(
            heavy.resolve(InputMask::HEAVY, InputMask::HEAVY, 40),
            ChordOutput::default()
        );
        assert_eq!(
            heavy.resolve(InputMask::AIM_GRAB, InputMask::NONE, 45),
            ChordOutput {
                heavy: true,
                grab: true,
                ..ChordOutput::default()
            }
        );
    }

    #[test]
    fn aim_grab_release_is_inclusive_but_holding_the_boundary_cancels() {
        let mut released_at_boundary = GuardChordTracker::default();
        assert_eq!(
            released_at_boundary.resolve(InputMask::AIM_GRAB, InputMask::AIM_GRAB, 100),
            ChordOutput::default()
        );
        for tick in 101..105 {
            assert_eq!(
                released_at_boundary.resolve(InputMask::NONE, InputMask::AIM_GRAB, tick),
                ChordOutput::default()
            );
        }
        assert_eq!(
            released_at_boundary.resolve(InputMask::NONE, InputMask::NONE, 105),
            ChordOutput {
                grab: true,
                ..ChordOutput::default()
            }
        );

        let mut held_at_boundary = GuardChordTracker::default();
        assert_eq!(
            held_at_boundary.resolve(InputMask::AIM_GRAB, InputMask::AIM_GRAB, 200),
            ChordOutput::default()
        );
        for tick in 201..=205 {
            assert_eq!(
                held_at_boundary.resolve(InputMask::NONE, InputMask::AIM_GRAB, tick),
                ChordOutput::default()
            );
        }
        assert_eq!(
            held_at_boundary.resolve(InputMask::NONE, InputMask::NONE, 206),
            ChordOutput::default(),
            "release after the held boundary cannot resurrect the cancelled tap"
        );
    }

    #[test]
    fn guard_and_ultimate_take_priority_over_pending_grab() {
        let mut guard = GuardChordTracker::default();
        assert_eq!(
            guard.resolve(InputMask::AIM_GRAB, InputMask::AIM_GRAB, 10),
            ChordOutput::default()
        );
        assert_eq!(
            guard.resolve(
                InputMask::LIGHT | InputMask::HEAVY,
                InputMask::LIGHT | InputMask::HEAVY,
                11,
            ),
            ChordOutput {
                guard: true,
                ..ChordOutput::default()
            }
        );

        let mut ultimate = GuardChordTracker::default();
        assert_eq!(
            ultimate.resolve(InputMask::AIM_GRAB, InputMask::AIM_GRAB, 20),
            ChordOutput::default()
        );
        assert_eq!(
            ultimate.resolve(InputMask::LIGHT, InputMask::AIM_GRAB | InputMask::LIGHT, 21,),
            ChordOutput::default()
        );
        let all = InputMask::AIM_GRAB | InputMask::LIGHT | InputMask::HEAVY;
        assert_eq!(
            ultimate.resolve(InputMask::HEAVY, all, 22),
            ChordOutput {
                ultimate: true,
                ..ChordOutput::default()
            }
        );
    }

    #[test]
    fn attack_on_grab_release_tick_is_staged_with_its_original_semantics() {
        let mut light = GuardChordTracker::default();
        assert_eq!(
            light.resolve(InputMask::AIM_GRAB, InputMask::AIM_GRAB, 10),
            ChordOutput::default()
        );
        assert_eq!(
            light.resolve(InputMask::LIGHT, InputMask::LIGHT, 12),
            ChordOutput {
                grab: true,
                ..ChordOutput::default()
            }
        );
        assert_eq!(
            light.resolve(InputMask::NONE, InputMask::LIGHT, 17),
            ChordOutput {
                light: true,
                ..ChordOutput::default()
            }
        );

        let mut heavy = GuardChordTracker::default();
        assert_eq!(
            heavy.resolve(InputMask::AIM_GRAB, InputMask::AIM_GRAB, 30),
            ChordOutput::default()
        );
        assert_eq!(
            heavy.resolve(InputMask::HEAVY, InputMask::HEAVY, 31),
            ChordOutput {
                grab: true,
                ..ChordOutput::default()
            }
        );
        assert_eq!(
            heavy.resolve(InputMask::NONE, InputMask::HEAVY, 36),
            ChordOutput {
                heavy: true,
                ..ChordOutput::default()
            }
        );
    }

    #[test]
    fn press_and_release_in_one_tick_still_produces_delayed_solo_action() {
        let mut tracker = GuardChordTracker::default();
        assert_eq!(
            tracker.resolve(InputMask::LIGHT, InputMask::NONE, 50),
            ChordOutput::default()
        );
        assert_eq!(
            tracker.resolve(InputMask::NONE, InputMask::NONE, 55),
            ChordOutput {
                light: true,
                ..ChordOutput::default()
            }
        );
    }

    #[test]
    fn gesture_reset_drops_pending_chords_without_changing_custom_windows() {
        let mut gestures = SeatGestureTrackers {
            dash: DashTapTracker::with_window(3),
            chord: GuardChordTracker::with_grace(2),
        };
        gestures.dash.register_presses(InputMask::LEFT, 1);
        gestures
            .chord
            .resolve(InputMask::HEAVY, InputMask::HEAVY, 1);

        gestures.reset();
        assert_eq!(gestures.dash.window_ticks(), 3);
        assert_eq!(gestures.chord.grace_ticks(), 2);
        assert!(
            gestures
                .dash
                .register_presses(InputMask::LEFT, 2)
                .is_empty()
        );
        assert_eq!(
            gestures.chord.resolve(InputMask::NONE, InputMask::NONE, 3),
            ChordOutput::default()
        );
    }
}
