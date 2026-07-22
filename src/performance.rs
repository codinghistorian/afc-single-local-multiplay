//! Opt-in runtime diagnostics for repeatable performance measurements.
//!
//! This module intentionally collects engine-level metrics only. Benchmark
//! scenarios remain explicit gameplay procedures so enabling `perf` cannot
//! modify match setup or fighter behavior.

use std::time::Duration;

use bevy::diagnostic::{
    EntityCountDiagnosticsPlugin, FrameTimeDiagnosticsPlugin, LogDiagnosticsPlugin,
};
use bevy::prelude::*;

/// Default interval between diagnostic snapshots written to the application log.
pub const DEFAULT_LOG_INTERVAL: Duration = Duration::from_secs(2);

/// Runtime settings for [`PerformancePlugin`].
#[derive(Resource, Debug, Clone)]
pub struct PerformanceConfig {
    /// Number of recent frame and entity-count samples retained for smoothing.
    pub history_length: usize,
    /// Interval between diagnostic snapshots written to the application log.
    pub log_interval: Duration,
}

impl Default for PerformanceConfig {
    fn default() -> Self {
        Self {
            history_length: 120,
            log_interval: DEFAULT_LOG_INTERVAL,
        }
    }
}

/// Registers frame-time, FPS, frame-count, and entity-count diagnostics.
pub struct PerformancePlugin {
    config: PerformanceConfig,
}

impl PerformancePlugin {
    pub fn new(config: PerformanceConfig) -> Self {
        Self { config }
    }
}

impl Default for PerformancePlugin {
    fn default() -> Self {
        Self::new(PerformanceConfig::default())
    }
}

impl Plugin for PerformancePlugin {
    fn build(&self, app: &mut App) {
        let history_length = self.config.history_length.max(1);

        app.insert_resource(self.config.clone()).add_plugins((
            FrameTimeDiagnosticsPlugin::new(history_length),
            EntityCountDiagnosticsPlugin::new(history_length),
            LogDiagnosticsPlugin {
                wait_duration: self.config.log_interval,
                ..default()
            },
        ));
    }
}
