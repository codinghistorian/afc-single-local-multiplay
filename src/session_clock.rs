//! Transport-independent authority-clock estimation and input-lead scheduling.
//!
//! The clock is presentation/session state, never canonical gameplay state. A
//! platform adapter supplies monotonic microseconds and echoes a probe identifier;
//! no system clock, timezone, or floating-point timestamp enters the simulation.

use core::fmt;

use crate::network_protocol::{
    MAX_INPUT_DELAY_TICKS, MIN_INPUT_DELAY_TICKS, SIMULATION_HZ, SimTick,
};

pub const CLOCK_SAMPLE_CAPACITY: usize = 8;
pub const MIN_CLOCK_SYNC_SAMPLES: u8 = 3;
pub const MAX_CLOCK_RTT_MICROS: u64 = 1_000_000;
pub const MAX_CLOCK_CATCH_UP_TICKS: u64 = 12;

/// One completed request/reply exchange. `authority_tick` is captured immediately
/// before the reply is queued. The client maps that instant to the local RTT
/// midpoint, selecting the lowest-RTT retained sample to limit queueing bias.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClockRoundTripSample {
    pub probe_id: u32,
    pub sent_micros: u64,
    pub received_micros: u64,
    pub authority_tick: SimTick,
}

impl ClockRoundTripSample {
    pub fn validate(self) -> Result<(), SessionClockError> {
        if self.probe_id == 0 {
            return Err(SessionClockError::ZeroProbeId);
        }
        let rtt = self
            .received_micros
            .checked_sub(self.sent_micros)
            .ok_or(SessionClockError::LocalClockRegressed)?;
        if rtt > MAX_CLOCK_RTT_MICROS {
            return Err(SessionClockError::RoundTripTooLarge { rtt_micros: rtt });
        }
        Ok(())
    }

    pub const fn rtt_micros(self) -> u64 {
        self.received_micros.saturating_sub(self.sent_micros)
    }

    pub const fn midpoint_micros(self) -> u64 {
        self.sent_micros + self.rtt_micros() / 2
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AuthorityTickEstimate {
    pub whole_tick: SimTick,
    /// Fraction of the following tick in unsigned Q0.16 form.
    pub sub_tick_q16: u16,
    pub best_rtt_micros: u32,
    pub uncertainty_micros: u32,
}

impl AuthorityTickEstimate {
    /// First tick that has not certainly started at the authority.
    pub fn ceiling_tick(self) -> Result<SimTick, SessionClockError> {
        let increment = u64::from(self.sub_tick_q16 != 0);
        self.whole_tick
            .0
            .checked_add(increment)
            .map(SimTick)
            .ok_or(SessionClockError::TimelineExhausted)
    }

    /// Maps the continuously advancing authority/network clock onto the gameplay
    /// timeline. AFC holds the canonical simulation at tick zero through loading
    /// and countdown; network tick `countdown_start_tick + N` corresponds to
    /// gameplay tick `N`. `None` means countdown has not reached the authority's
    /// selected boundary yet.
    pub fn for_match(self, countdown_start_tick: SimTick) -> Option<Self> {
        if self.whole_tick < countdown_start_tick {
            return None;
        }
        Some(Self {
            whole_tick: SimTick(self.whole_tick.0 - countdown_start_tick.0),
            ..self
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SessionClockMetrics {
    pub accepted_samples: u64,
    pub duplicate_samples: u64,
    pub rejected_samples: u64,
    pub best_rtt_micros: u32,
    pub maximum_rtt_micros: u32,
    pub synchronized_transitions: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionClockError {
    ZeroProbeId,
    DuplicateProbeConflict,
    LocalClockRegressed,
    RoundTripTooLarge { rtt_micros: u64 },
    NotSynchronized,
    InvalidInputDelay,
    InputSchedulerRegressed,
    TimelineExhausted,
}

impl fmt::Display for SessionClockError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid multiplayer session clock: {self:?}")
    }
}

impl std::error::Error for SessionClockError {}

/// Fixed-capacity estimator. Retaining multiple samples proves the connection is
/// live; choosing the minimum RTT sample avoids averaging asymmetric queue spikes
/// into the simulation-tick mapping.
pub struct AuthorityClockSynchronizer {
    samples: [ClockRoundTripSample; CLOCK_SAMPLE_CAPACITY],
    len: u8,
    cursor: u8,
    best_index: u8,
    synchronized: bool,
    metrics: SessionClockMetrics,
}

impl Default for AuthorityClockSynchronizer {
    fn default() -> Self {
        Self {
            samples: [ClockRoundTripSample::default(); CLOCK_SAMPLE_CAPACITY],
            len: 0,
            cursor: 0,
            best_index: 0,
            synchronized: false,
            metrics: SessionClockMetrics::default(),
        }
    }
}

impl AuthorityClockSynchronizer {
    pub const fn is_synchronized(&self) -> bool {
        self.synchronized
    }

    pub const fn metrics(&self) -> SessionClockMetrics {
        self.metrics
    }

    pub fn observe(&mut self, sample: ClockRoundTripSample) -> Result<bool, SessionClockError> {
        if let Err(error) = sample.validate() {
            self.metrics.rejected_samples = self.metrics.rejected_samples.saturating_add(1);
            return Err(error);
        }

        if let Some(existing) = self.samples[..usize::from(self.len)]
            .iter()
            .find(|candidate| candidate.probe_id == sample.probe_id)
        {
            if *existing == sample {
                self.metrics.duplicate_samples = self.metrics.duplicate_samples.saturating_add(1);
                return Ok(false);
            }
            self.metrics.rejected_samples = self.metrics.rejected_samples.saturating_add(1);
            return Err(SessionClockError::DuplicateProbeConflict);
        }

        let insertion = usize::from(self.cursor);
        self.samples[insertion] = sample;
        if usize::from(self.len) < CLOCK_SAMPLE_CAPACITY {
            self.len += 1;
        }
        self.cursor = ((insertion + 1) % CLOCK_SAMPLE_CAPACITY) as u8;
        self.reselect_best();

        let rtt = sample.rtt_micros().min(u64::from(u32::MAX)) as u32;
        self.metrics.accepted_samples = self.metrics.accepted_samples.saturating_add(1);
        self.metrics.maximum_rtt_micros = self.metrics.maximum_rtt_micros.max(rtt);
        self.metrics.best_rtt_micros = self.best_sample().rtt_micros() as u32;

        let became_synchronized = !self.synchronized && self.len >= MIN_CLOCK_SYNC_SAMPLES;
        if became_synchronized {
            self.synchronized = true;
            self.metrics.synchronized_transitions =
                self.metrics.synchronized_transitions.saturating_add(1);
        }
        Ok(became_synchronized)
    }

    pub fn estimate(&self, now_micros: u64) -> Result<AuthorityTickEstimate, SessionClockError> {
        if !self.synchronized {
            return Err(SessionClockError::NotSynchronized);
        }
        let reference = self.best_sample();
        let midpoint = reference.midpoint_micros();
        let elapsed = now_micros
            .checked_sub(midpoint)
            .ok_or(SessionClockError::LocalClockRegressed)?;

        let elapsed_tick_numerator = u128::from(elapsed) * u128::from(SIMULATION_HZ);
        let whole_elapsed = elapsed_tick_numerator / 1_000_000;
        let remainder = elapsed_tick_numerator % 1_000_000;
        let whole_tick = u128::from(reference.authority_tick.0)
            .checked_add(whole_elapsed)
            .filter(|tick| *tick <= u128::from(u64::MAX))
            .ok_or(SessionClockError::TimelineExhausted)? as u64;
        let sub_tick_q16 = ((remainder << 16) / 1_000_000) as u16;
        let best_rtt = reference.rtt_micros();
        let uncertainty =
            (best_rtt / 2).saturating_add(1_000_000_u64.div_ceil(u64::from(SIMULATION_HZ)));

        Ok(AuthorityTickEstimate {
            whole_tick: SimTick(whole_tick),
            sub_tick_q16,
            best_rtt_micros: best_rtt.min(u64::from(u32::MAX)) as u32,
            uncertainty_micros: uncertainty.min(u64::from(u32::MAX)) as u32,
        })
    }

    fn best_sample(&self) -> ClockRoundTripSample {
        self.samples[usize::from(self.best_index)]
    }

    fn reselect_best(&mut self) {
        let mut best = 0_usize;
        for index in 1..usize::from(self.len) {
            let candidate = self.samples[index];
            let selected = self.samples[best];
            if (candidate.rtt_micros(), candidate.probe_id)
                < (selected.rtt_micros(), selected.probe_id)
            {
                best = index;
            }
        }
        self.best_index = best as u8;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DueInputTicks {
    pub first: SimTick,
    pub last: SimTick,
}

impl DueInputTicks {
    pub const fn len(self) -> u64 {
        self.last.0 - self.first.0 + 1
    }
}

/// Converts a synchronized authority-clock estimate into a monotonic sequence of
/// input ticks. If a client render frame stalls, the caller receives a bounded
/// contiguous catch-up range and samples its current action state for every tick.
pub struct InputLeadScheduler {
    input_delay_ticks: u8,
    last_emitted: Option<SimTick>,
}

impl InputLeadScheduler {
    pub fn new(input_delay_ticks: u8) -> Result<Self, SessionClockError> {
        Self::from_confirmed_tick(input_delay_ticks, SimTick::ZERO)
    }

    /// Starts or resets scheduling after applying an exact canonical snapshot.
    /// The next range always begins at `confirmed_tick + 1`, including the initial
    /// tick-one frames used to establish prediction lead during countdown exit.
    pub fn from_confirmed_tick(
        input_delay_ticks: u8,
        confirmed_tick: SimTick,
    ) -> Result<Self, SessionClockError> {
        if !(MIN_INPUT_DELAY_TICKS..=MAX_INPUT_DELAY_TICKS).contains(&input_delay_ticks) {
            return Err(SessionClockError::InvalidInputDelay);
        }
        Ok(Self {
            input_delay_ticks,
            last_emitted: Some(confirmed_tick),
        })
    }

    pub const fn last_emitted(&self) -> Option<SimTick> {
        self.last_emitted
    }

    pub fn due_ticks(
        &mut self,
        estimate: AuthorityTickEstimate,
    ) -> Result<Option<DueInputTicks>, SessionClockError> {
        let target = estimate
            .ceiling_tick()?
            .0
            .checked_add(u64::from(self.input_delay_ticks))
            .map(SimTick)
            .ok_or(SessionClockError::TimelineExhausted)?;

        let first = match self.last_emitted {
            None => SimTick(1),
            Some(last) if target.0 <= last.0 => return Ok(None),
            Some(last) => last
                .0
                .checked_add(1)
                .map(SimTick)
                .ok_or(SessionClockError::TimelineExhausted)?,
        };
        // A repair snapshot or a temporarily stalled render/client worker can
        // legitimately leave more than one rollback window of local input to
        // sample. Keep each service operation bounded without turning that
        // recoverable backlog into a fatal clock error; subsequent pumps drain
        // the remaining contiguous ticks in equally bounded chunks.
        let last = SimTick(
            target
                .0
                .min(first.0.saturating_add(MAX_CLOCK_CATCH_UP_TICKS - 1)),
        );
        self.last_emitted = Some(last);
        Ok(Some(DueInputTicks { first, last }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(probe_id: u32, sent: u64, rtt: u64, authority_tick: u64) -> ClockRoundTripSample {
        ClockRoundTripSample {
            probe_id,
            sent_micros: sent,
            received_micros: sent + rtt,
            authority_tick: SimTick(authority_tick),
        }
    }

    #[test]
    fn requires_multiple_samples_and_selects_lowest_rtt_reference() {
        let mut clock = AuthorityClockSynchronizer::default();
        assert!(!clock.observe(sample(1, 1_000_000, 80_000, 60)).unwrap());
        assert!(!clock.observe(sample(2, 2_000_000, 20_000, 120)).unwrap());
        assert!(clock.observe(sample(3, 3_000_000, 50_000, 180)).unwrap());

        let estimate = clock.estimate(2_510_000).unwrap();
        assert_eq!(estimate.whole_tick, SimTick(150));
        assert_eq!(estimate.sub_tick_q16, 0);
        assert_eq!(estimate.best_rtt_micros, 20_000);
        assert_eq!(clock.metrics().synchronized_transitions, 1);
    }

    #[test]
    fn estimate_retains_fraction_without_floating_point() {
        let mut clock = AuthorityClockSynchronizer::default();
        for probe in 1..=3 {
            clock
                .observe(sample(probe, 1_000_000 + u64::from(probe), 0, 60))
                .unwrap();
        }
        let midpoint = 1_000_001;
        let estimate = clock.estimate(midpoint + 8_333).unwrap();
        assert_eq!(estimate.whole_tick, SimTick(60));
        assert!(estimate.sub_tick_q16 > 32_000 && estimate.sub_tick_q16 < 33_000);
    }

    #[test]
    fn invalid_and_conflicting_samples_fail_closed() {
        let mut clock = AuthorityClockSynchronizer::default();
        assert_eq!(
            clock.observe(sample(0, 10, 1, 1)),
            Err(SessionClockError::ZeroProbeId)
        );
        let accepted = sample(1, 10, 4, 1);
        clock.observe(accepted).unwrap();
        assert!(!clock.observe(accepted).unwrap());
        assert_eq!(
            clock.observe(sample(1, 10, 5, 1)),
            Err(SessionClockError::DuplicateProbeConflict)
        );
        assert_eq!(clock.metrics().duplicate_samples, 1);
        assert_eq!(clock.metrics().rejected_samples, 2);
    }

    #[test]
    fn input_scheduler_emits_contiguous_catch_up_and_never_duplicates() {
        let mut scheduler = InputLeadScheduler::new(2).unwrap();
        let at_start = AuthorityTickEstimate {
            whole_tick: SimTick(0),
            ..AuthorityTickEstimate::default()
        };
        assert_eq!(
            scheduler.due_ticks(at_start).unwrap(),
            Some(DueInputTicks {
                first: SimTick(1),
                last: SimTick(2)
            })
        );
        assert_eq!(scheduler.due_ticks(at_start).unwrap(), None);

        let at_four = AuthorityTickEstimate {
            whole_tick: SimTick(4),
            sub_tick_q16: 1,
            ..AuthorityTickEstimate::default()
        };
        assert_eq!(
            scheduler.due_ticks(at_four).unwrap(),
            Some(DueInputTicks {
                first: SimTick(3),
                last: SimTick(7)
            })
        );
    }

    #[test]
    fn input_scheduler_delay_one_through_six_and_subtick_boundary_are_exact() {
        for delay in MIN_INPUT_DELAY_TICKS..=MAX_INPUT_DELAY_TICKS {
            let mut scheduler =
                InputLeadScheduler::from_confirmed_tick(delay, SimTick(100)).unwrap();
            assert_eq!(
                scheduler
                    .due_ticks(AuthorityTickEstimate {
                        whole_tick: SimTick(100),
                        sub_tick_q16: 0,
                        ..AuthorityTickEstimate::default()
                    })
                    .unwrap(),
                Some(DueInputTicks {
                    first: SimTick(101),
                    last: SimTick(100 + u64::from(delay)),
                }),
                "integer boundary for delay {delay}"
            );
            assert_eq!(
                scheduler
                    .due_ticks(AuthorityTickEstimate {
                        whole_tick: SimTick(100),
                        sub_tick_q16: 1,
                        ..AuthorityTickEstimate::default()
                    })
                    .unwrap(),
                Some(DueInputTicks {
                    first: SimTick(101 + u64::from(delay)),
                    last: SimTick(101 + u64::from(delay)),
                }),
                "first positive subtick for delay {delay}"
            );
            assert_eq!(
                scheduler
                    .due_ticks(AuthorityTickEstimate {
                        whole_tick: SimTick(100),
                        sub_tick_q16: u16::MAX,
                        ..AuthorityTickEstimate::default()
                    })
                    .unwrap(),
                None,
                "largest subtick remains in the same ceiling tick for delay {delay}"
            );
        }
        assert!(matches!(
            InputLeadScheduler::new(MIN_INPUT_DELAY_TICKS - 1),
            Err(SessionClockError::InvalidInputDelay)
        ));
        assert!(matches!(
            InputLeadScheduler::new(MAX_INPUT_DELAY_TICKS + 1),
            Err(SessionClockError::InvalidInputDelay)
        ));
    }

    #[test]
    fn network_clock_maps_to_zero_based_gameplay_clock_at_start() {
        let countdown = AuthorityTickEstimate {
            whole_tick: SimTick(99),
            sub_tick_q16: u16::MAX,
            ..AuthorityTickEstimate::default()
        };
        assert_eq!(countdown.for_match(SimTick(100)), None);

        let running = AuthorityTickEstimate {
            whole_tick: SimTick(104),
            sub_tick_q16: 123,
            ..AuthorityTickEstimate::default()
        }
        .for_match(SimTick(100))
        .unwrap();
        assert_eq!(running.whole_tick, SimTick(4));
        assert_eq!(running.sub_tick_q16, 123);
    }

    #[test]
    fn excessive_scheduler_jump_is_drained_in_bounded_contiguous_chunks() {
        let mut scheduler = InputLeadScheduler::new(1).unwrap();
        scheduler
            .due_ticks(AuthorityTickEstimate {
                whole_tick: SimTick(1),
                ..AuthorityTickEstimate::default()
            })
            .unwrap();
        assert_eq!(
            scheduler
                .due_ticks(AuthorityTickEstimate {
                    whole_tick: SimTick(30),
                    ..AuthorityTickEstimate::default()
                })
                .unwrap(),
            Some(DueInputTicks {
                first: SimTick(3),
                last: SimTick(14),
            })
        );
        assert_eq!(
            scheduler
                .due_ticks(AuthorityTickEstimate {
                    whole_tick: SimTick(30),
                    ..AuthorityTickEstimate::default()
                })
                .unwrap(),
            Some(DueInputTicks {
                first: SimTick(15),
                last: SimTick(26),
            })
        );
        assert_eq!(
            scheduler
                .due_ticks(AuthorityTickEstimate {
                    whole_tick: SimTick(30),
                    ..AuthorityTickEstimate::default()
                })
                .unwrap(),
            Some(DueInputTicks {
                first: SimTick(27),
                last: SimTick(31),
            })
        );
    }
}
