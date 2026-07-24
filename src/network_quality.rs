//! Bounded, transport-independent network-quality classification.
//!
//! Samples and policy live outside canonical gameplay. They drive UI warnings,
//! matchmaking admission, and disconnect decisions without changing simulation
//! results or rollback limits mid-match.

use core::fmt;

pub const QUALITY_WINDOW_SAMPLES: usize = 120;
pub const DEFAULT_QUALITY_TRANSITION_SAMPLES: u16 = 30;
pub const DEFAULT_QUALITY_RECOVERY_SAMPLES: u16 = 120;
pub const PRECOMMIT_RTT_SAMPLE_CAPACITY: usize = 32;
pub const MIN_PRECOMMIT_RTT_SAMPLES: usize = 20;
pub const MIN_CALIBRATED_INPUT_DELAY_TICKS: u8 = 2;
pub const MAX_CALIBRATED_INPUT_DELAY_TICKS: u8 = 6;
pub const MAX_CALIBRATED_ROLLBACK_TICKS: u16 = 12;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InputDelayCalibrationState {
    #[default]
    NotAuthority,
    Calibrating,
    Ready,
    Unplayable,
    Committed,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputDelayCalibrationSnapshot {
    pub state: InputDelayCalibrationState,
    pub remote_peer_count: u8,
    pub calibrated_peer_count: u8,
    pub worst_p95_rtt_ms: Option<u16>,
    pub selected_input_delay_ticks: Option<u8>,
    pub required_rollback_ticks: Option<u16>,
}

/// Fixed-capacity RTT sampler used only while a listen authority is preparing
/// an immutable match manifest.
///
/// Unknown Steam ping readings are deliberately skipped. They are not latency
/// samples and must never enter calibration as a synthetic zero-RTT reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrecommitRttCalibrator {
    samples: [u16; PRECOMMIT_RTT_SAMPLE_CAPACITY],
    cursor: usize,
    len: usize,
    cached_p95_rtt_ms: Option<u16>,
}

impl Default for PrecommitRttCalibrator {
    fn default() -> Self {
        Self {
            samples: [0; PRECOMMIT_RTT_SAMPLE_CAPACITY],
            cursor: 0,
            len: 0,
            cached_p95_rtt_ms: None,
        }
    }
}

impl PrecommitRttCalibrator {
    pub fn reset(&mut self) {
        self.samples = [0; PRECOMMIT_RTT_SAMPLE_CAPACITY];
        self.cursor = 0;
        self.len = 0;
        self.cached_p95_rtt_ms = None;
    }

    /// Records one valid Steam RTT sample. Returns `true` only when a sample was
    /// recorded; `None` leaves the current connection generation unchanged.
    pub fn observe(&mut self, ping_ms: Option<u32>) -> bool {
        let Some(ping_ms) = ping_ms else {
            return false;
        };
        self.samples[self.cursor] = ping_ms.min(u32::from(u16::MAX)) as u16;
        self.cursor = (self.cursor + 1) % PRECOMMIT_RTT_SAMPLE_CAPACITY;
        self.len = (self.len + 1).min(PRECOMMIT_RTT_SAMPLE_CAPACITY);
        self.cached_p95_rtt_ms = self.calculate_p95_rtt_ms();
        true
    }

    pub const fn sample_count(&self) -> usize {
        self.len
    }

    pub const fn is_ready(&self) -> bool {
        self.len >= MIN_PRECOMMIT_RTT_SAMPLES
    }

    /// Nearest-rank p95 over the current connection generation.
    pub const fn p95_rtt_ms(&self) -> Option<u16> {
        self.cached_p95_rtt_ms
    }

    fn calculate_p95_rtt_ms(&self) -> Option<u16> {
        if !self.is_ready() {
            return None;
        }
        let mut sorted = [0_u16; PRECOMMIT_RTT_SAMPLE_CAPACITY];
        sorted[..self.len].copy_from_slice(&self.samples[..self.len]);
        sorted[..self.len].sort_unstable();
        let nearest_rank = (95 * self.len).div_ceil(100);
        Some(sorted[nearest_rank.saturating_sub(1)])
    }
}

/// Converts a measured p95 RTT to the immutable input lead and rollback budget
/// required by the 60 Hz protocol.
pub const fn calibrated_input_delay(p95_rtt_ms: u16) -> (u8, u16) {
    let half_rtt_ticks = ((p95_rtt_ms as u32) * 60).div_ceil(2_000) as u16;
    let unclamped_delay = half_rtt_ticks.saturating_add(1);
    let delay = if unclamped_delay < MIN_CALIBRATED_INPUT_DELAY_TICKS as u16 {
        MIN_CALIBRATED_INPUT_DELAY_TICKS
    } else if unclamped_delay > MAX_CALIBRATED_INPUT_DELAY_TICKS as u16 {
        MAX_CALIBRATED_INPUT_DELAY_TICKS
    } else {
        unclamped_delay as u8
    };
    (delay, half_rtt_ticks.saturating_add(delay as u16))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NetworkQualityPolicy {
    pub preferred_rtt_ms: u16,
    pub degraded_rtt_ms: u16,
    pub reject_rtt_ms: u16,
    /// Packet loss in basis points, where 100 basis points is 1%.
    pub degraded_loss_bps: u16,
    pub reject_loss_bps: u16,
    pub transition_samples: u16,
    pub recovery_samples: u16,
}

impl Default for NetworkQualityPolicy {
    fn default() -> Self {
        Self {
            preferred_rtt_ms: 100,
            degraded_rtt_ms: 150,
            reject_rtt_ms: 250,
            degraded_loss_bps: 300,
            reject_loss_bps: 1_000,
            transition_samples: DEFAULT_QUALITY_TRANSITION_SAMPLES,
            recovery_samples: DEFAULT_QUALITY_RECOVERY_SAMPLES,
        }
    }
}

impl NetworkQualityPolicy {
    pub const fn validate(self) -> Result<(), NetworkQualityError> {
        if self.preferred_rtt_ms == 0
            || self.preferred_rtt_ms >= self.degraded_rtt_ms
            || self.degraded_rtt_ms >= self.reject_rtt_ms
            || self.degraded_loss_bps == 0
            || self.degraded_loss_bps >= self.reject_loss_bps
            || self.reject_loss_bps > 10_000
            || self.transition_samples == 0
            || self.transition_samples as usize > QUALITY_WINDOW_SAMPLES
            || self.recovery_samples == 0
        {
            Err(NetworkQualityError::InvalidPolicy)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum NetworkQuality {
    #[default]
    Healthy,
    Warning,
    Degraded,
    Reject,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NetworkQualitySample {
    pub rtt_ms: u16,
    pub loss_bps: u16,
}

impl NetworkQualitySample {
    pub const fn validate(self) -> Result<(), NetworkQualityError> {
        if self.loss_bps > 10_000 {
            Err(NetworkQualityError::InvalidSample)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NetworkQualitySnapshot {
    pub quality: NetworkQuality,
    pub sample_count: u16,
    pub average_rtt_ms: u16,
    pub average_loss_bps: u16,
    pub peak_rtt_ms: u16,
    pub peak_loss_bps: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetworkQualityError {
    InvalidPolicy,
    InvalidSample,
}

impl fmt::Display for NetworkQualityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid network-quality input: {self:?}")
    }
}

impl std::error::Error for NetworkQualityError {}

/// Allocation-free rolling classifier with explicit transition hysteresis.
pub struct NetworkQualityMonitor {
    policy: NetworkQualityPolicy,
    samples: [NetworkQualitySample; QUALITY_WINDOW_SAMPLES],
    cursor: usize,
    len: usize,
    rtt_sum: u64,
    loss_sum: u64,
    quality: NetworkQuality,
    pending: NetworkQuality,
    pending_samples: u16,
}

impl NetworkQualityMonitor {
    pub fn new(policy: NetworkQualityPolicy) -> Result<Self, NetworkQualityError> {
        policy.validate()?;
        Ok(Self {
            policy,
            samples: [NetworkQualitySample::default(); QUALITY_WINDOW_SAMPLES],
            cursor: 0,
            len: 0,
            rtt_sum: 0,
            loss_sum: 0,
            quality: NetworkQuality::Healthy,
            pending: NetworkQuality::Healthy,
            pending_samples: 0,
        })
    }

    pub const fn policy(&self) -> NetworkQualityPolicy {
        self.policy
    }

    pub const fn quality(&self) -> NetworkQuality {
        self.quality
    }

    pub fn observe(
        &mut self,
        sample: NetworkQualitySample,
    ) -> Result<NetworkQualitySnapshot, NetworkQualityError> {
        sample.validate()?;
        if self.len == QUALITY_WINDOW_SAMPLES {
            let replaced = self.samples[self.cursor];
            self.rtt_sum = self.rtt_sum.saturating_sub(u64::from(replaced.rtt_ms));
            self.loss_sum = self.loss_sum.saturating_sub(u64::from(replaced.loss_bps));
        } else {
            self.len += 1;
        }
        self.samples[self.cursor] = sample;
        self.cursor = (self.cursor + 1) % QUALITY_WINDOW_SAMPLES;
        self.rtt_sum = self.rtt_sum.saturating_add(u64::from(sample.rtt_ms));
        self.loss_sum = self.loss_sum.saturating_add(u64::from(sample.loss_bps));

        let desired = self.desired_quality();
        if desired == self.quality {
            self.pending = desired;
            self.pending_samples = 0;
        } else if desired != self.pending {
            self.pending = desired;
            self.pending_samples = 1;
        } else {
            self.pending_samples = self.pending_samples.saturating_add(1);
        }

        let required = if desired > self.quality {
            self.policy.transition_samples
        } else {
            self.policy.recovery_samples
        };
        if desired != self.quality && self.pending_samples >= required {
            self.quality = desired;
            self.pending_samples = 0;
        }
        Ok(self.snapshot())
    }

    pub fn snapshot(&self) -> NetworkQualitySnapshot {
        let divisor = self.len.max(1) as u64;
        let (peak_rtt_ms, peak_loss_bps) = self.samples[..self.len]
            .iter()
            .fold((0_u16, 0_u16), |(rtt, loss), sample| {
                (rtt.max(sample.rtt_ms), loss.max(sample.loss_bps))
            });
        NetworkQualitySnapshot {
            quality: self.quality,
            sample_count: self.len as u16,
            average_rtt_ms: (self.rtt_sum / divisor).min(u64::from(u16::MAX)) as u16,
            average_loss_bps: (self.loss_sum / divisor).min(u64::from(u16::MAX)) as u16,
            peak_rtt_ms,
            peak_loss_bps,
        }
    }

    fn desired_quality(&self) -> NetworkQuality {
        let snapshot = self.snapshot();
        if snapshot.average_rtt_ms > self.policy.reject_rtt_ms
            || snapshot.average_loss_bps > self.policy.reject_loss_bps
        {
            NetworkQuality::Reject
        } else if snapshot.average_rtt_ms > self.policy.degraded_rtt_ms
            || snapshot.average_loss_bps > self.policy.degraded_loss_bps
        {
            NetworkQuality::Degraded
        } else if snapshot.average_rtt_ms > self.policy.preferred_rtt_ms {
            NetworkQuality::Warning
        } else {
            NetworkQuality::Healthy
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(transition: u16, recovery: u16) -> NetworkQualityMonitor {
        NetworkQualityMonitor::new(NetworkQualityPolicy {
            transition_samples: transition,
            recovery_samples: recovery,
            ..NetworkQualityPolicy::default()
        })
        .unwrap()
    }

    #[test]
    fn one_transient_sample_does_not_change_quality() {
        let mut monitor = monitor(3, 4);
        monitor
            .observe(NetworkQualitySample {
                rtt_ms: 400,
                loss_bps: 2_000,
            })
            .unwrap();
        assert_eq!(monitor.quality(), NetworkQuality::Healthy);
    }

    #[test]
    fn sustained_thresholds_transition_and_recover_with_hysteresis() {
        let mut monitor = monitor(2, 3);
        for _ in 0..2 {
            monitor
                .observe(NetworkQualitySample {
                    rtt_ms: 251,
                    loss_bps: 0,
                })
                .unwrap();
        }
        assert_eq!(monitor.quality(), NetworkQuality::Reject);

        // The rolling average remains above the threshold until enough healthy
        // samples replace it, and then the recovery streak must also complete.
        for _ in 0..QUALITY_WINDOW_SAMPLES + 2 {
            monitor
                .observe(NetworkQualitySample {
                    rtt_ms: 40,
                    loss_bps: 0,
                })
                .unwrap();
        }
        assert_eq!(monitor.quality(), NetworkQuality::Healthy);
    }

    #[test]
    fn warning_degraded_and_loss_boundaries_are_explicit() {
        let mut warning = monitor(1, 1);
        assert_eq!(
            warning
                .observe(NetworkQualitySample {
                    rtt_ms: 101,
                    loss_bps: 0,
                })
                .unwrap()
                .quality,
            NetworkQuality::Warning
        );

        let mut degraded = monitor(1, 1);
        assert_eq!(
            degraded
                .observe(NetworkQualitySample {
                    rtt_ms: 151,
                    loss_bps: 0,
                })
                .unwrap()
                .quality,
            NetworkQuality::Degraded
        );

        let mut loss = monitor(1, 1);
        assert_eq!(
            loss.observe(NetworkQualitySample {
                rtt_ms: 40,
                loss_bps: 301,
            })
            .unwrap()
            .quality,
            NetworkQuality::Degraded
        );
    }

    #[test]
    fn sample_storage_is_fixed_and_invalid_loss_is_rejected() {
        let mut monitor = monitor(1, 1);
        for index in 0..QUALITY_WINDOW_SAMPLES * 3 {
            monitor
                .observe(NetworkQualitySample {
                    rtt_ms: (index % 200) as u16,
                    loss_bps: 0,
                })
                .unwrap();
        }
        assert_eq!(
            monitor.snapshot().sample_count as usize,
            QUALITY_WINDOW_SAMPLES
        );
        assert_eq!(
            monitor.observe(NetworkQualitySample {
                rtt_ms: 1,
                loss_bps: 10_001,
            }),
            Err(NetworkQualityError::InvalidSample)
        );
    }

    #[test]
    fn precommit_calibration_skips_unknown_ping_and_uses_nearest_rank_p95() {
        let mut calibrator = PrecommitRttCalibrator::default();
        for _ in 0..PRECOMMIT_RTT_SAMPLE_CAPACITY {
            assert!(!calibrator.observe(None));
        }
        assert_eq!(calibrator.sample_count(), 0);
        assert_eq!(calibrator.p95_rtt_ms(), None);

        for sample in 1..=MIN_PRECOMMIT_RTT_SAMPLES as u32 {
            assert!(calibrator.observe(Some(sample)));
        }
        // nearest-rank ceil(0.95 * 20) selects sample 19, not the peak.
        assert_eq!(calibrator.p95_rtt_ms(), Some(19));
    }

    #[test]
    fn precommit_calibration_is_bounded_and_reset_per_connection_generation() {
        let mut calibrator = PrecommitRttCalibrator::default();
        for sample in 0..PRECOMMIT_RTT_SAMPLE_CAPACITY * 3 {
            calibrator.observe(Some(sample as u32));
        }
        assert_eq!(calibrator.sample_count(), PRECOMMIT_RTT_SAMPLE_CAPACITY);
        assert_eq!(calibrator.p95_rtt_ms(), Some(94));

        calibrator.reset();
        assert_eq!(calibrator.sample_count(), 0);
        assert_eq!(calibrator.p95_rtt_ms(), None);
        for _ in 0..MIN_PRECOMMIT_RTT_SAMPLES {
            calibrator.observe(Some(200));
        }
        assert_eq!(calibrator.p95_rtt_ms(), Some(200));
    }

    #[test]
    fn calibrated_delay_profiles_and_unplayable_boundary_are_exact() {
        assert_eq!(calibrated_input_delay(0), (2, 2));
        assert_eq!(calibrated_input_delay(100), (4, 7));
        assert_eq!(calibrated_input_delay(120), (5, 9));
        assert_eq!(calibrated_input_delay(200), (6, 12));
        assert_eq!(calibrated_input_delay(201), (6, 13));
        assert!(calibrated_input_delay(200).1 <= MAX_CALIBRATED_ROLLBACK_TICKS);
        assert!(calibrated_input_delay(201).1 > MAX_CALIBRATED_ROLLBACK_TICKS);
    }
}
