//! Render-only interpolation for authoritative simulation poses.
//!
//! Gameplay stores fighter positions in [`SimPosition`]. This module provides
//! the one-way hand-off into Bevy render `Transform` components:
//!
//! - fixed gameplay mutates only the authoritative position.
//! - the end of the fixed tick captures it.
//! - `Update` temporarily writes an interpolated render position.
//!
//! Snapshots, hashes, rollback, and gameplay read [`SimPosition`], never the
//! temporarily interpolated `Transform` value.

use bevy::prelude::*;

use crate::components::{Fighter, SimPosition};

/// A displacement this large in one tick is treated as a teleport rather than
/// ordinary movement. Respawns and hard corrections therefore render at their
/// committed position without streaking across the arena.
const TELEPORT_SNAP_DISTANCE: f32 = 1.5;

/// Authoritative translation history for one render proxy.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct SimPoseHistory {
    pub previous: Vec3,
    pub current: Vec3,
}

impl SimPoseHistory {
    pub const fn new(position: Vec3) -> Self {
        Self {
            previous: position,
            current: position,
        }
    }

    pub fn begin_tick(&mut self) {
        self.previous = self.current;
    }

    pub fn capture(&mut self, position: Vec3) {
        if position.distance_squared(self.current) > TELEPORT_SNAP_DISTANCE * TELEPORT_SNAP_DISTANCE
        {
            self.snap(position);
        } else {
            self.current = position;
        }
    }

    pub fn snap(&mut self, position: Vec3) {
        self.previous = position;
        self.current = position;
    }

    pub fn interpolated(self, alpha: f32) -> Vec3 {
        self.previous.lerp(self.current, alpha.clamp(0.0, 1.0))
    }
}

/// Explicit snap request for match reset or rollback restoration.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SimPoseSnapRequest {
    requested: bool,
}

impl SimPoseSnapRequest {
    pub const fn requested() -> Self {
        Self { requested: true }
    }

    pub fn request(&mut self) {
        self.requested = true;
    }

    pub const fn is_requested(self) -> bool {
        self.requested
    }
}

/// Adds pose history to fighters created after application startup.
pub fn initialize_sim_pose_history(
    mut commands: Commands,
    fighters: Query<(Entity, &SimPosition), (With<Fighter>, Without<SimPoseHistory>)>,
) {
    for (entity, position) in &fighters {
        commands
            .entity(entity)
            .insert(SimPoseHistory::new(position.translation));
    }
}

/// Restores the committed pose into the render proxy before fixed steps.
/// Authoritative systems do not read this `Transform` copy.
pub fn restore_committed_sim_poses(
    mut fighters: Query<(&SimPosition, &mut Transform), With<Fighter>>,
) {
    for (position, mut transform) in &mut fighters {
        transform.translation = position.translation;
    }
}

/// Rolls the pose history forward at the beginning of each fixed tick.
pub fn begin_sim_pose_tick(
    mut fighters: Query<(&mut SimPoseHistory, &SimPosition), With<Fighter>>,
) {
    for (mut history, position) in &mut fighters {
        history.begin_tick();
        debug_assert_eq!(history.current, position.translation);
    }
}

/// Commits positions produced by the fixed gameplay pipeline.
pub fn capture_sim_pose_tick(
    mut fighters: Query<(&mut SimPoseHistory, &SimPosition), With<Fighter>>,
) {
    for (mut history, position) in &mut fighters {
        history.capture(position.translation);
    }
}

/// Snaps histories after an explicit reset or authoritative correction.
///
/// Schedule this after the system applying the reset/correction and before
/// [`interpolate_sim_poses`].
pub fn apply_sim_pose_snap_request(
    mut request: ResMut<SimPoseSnapRequest>,
    mut fighters: Query<(&mut SimPoseHistory, &SimPosition), With<Fighter>>,
) {
    if !request.requested {
        return;
    }

    for (mut history, position) in &mut fighters {
        history.snap(position.translation);
    }
    request.requested = false;
}

/// Writes render-only translations using Bevy's fixed-clock overstep fraction.
pub fn interpolate_sim_poses(
    fixed_time: Res<Time<Fixed>>,
    mut fighters: Query<(&SimPoseHistory, &mut Transform), With<Fighter>>,
) {
    let alpha = fixed_time.overstep_fraction();
    for (history, mut transform) in &mut fighters {
        transform.translation = history.interpolated(alpha);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolation_never_mutates_committed_positions() {
        let history = SimPoseHistory {
            previous: Vec3::new(1.0, 2.0, 3.0),
            current: Vec3::new(5.0, 6.0, 7.0),
        };

        assert_eq!(history.interpolated(0.25), Vec3::new(2.0, 3.0, 4.0));
        assert_eq!(history.current, Vec3::new(5.0, 6.0, 7.0));
        assert_eq!(history.previous, Vec3::new(1.0, 2.0, 3.0));
    }

    #[test]
    fn ordinary_motion_rolls_previous_and_current_forward() {
        let mut history = SimPoseHistory::new(Vec3::ZERO);
        history.capture(Vec3::new(0.25, 0.0, 0.0));
        history.begin_tick();
        history.capture(Vec3::new(0.5, 0.0, 0.0));

        assert_eq!(history.previous, Vec3::new(0.25, 0.0, 0.0));
        assert_eq!(history.current, Vec3::new(0.5, 0.0, 0.0));
    }

    #[test]
    fn teleports_snap_both_history_ends() {
        let mut history = SimPoseHistory::new(Vec3::ZERO);
        let destination = Vec3::new(8.0, 0.0, -4.0);
        history.capture(destination);

        assert_eq!(history.previous, destination);
        assert_eq!(history.current, destination);
        assert_eq!(history.interpolated(0.5), destination);
    }

    #[test]
    fn interpolation_alpha_is_clamped() {
        let history = SimPoseHistory {
            previous: Vec3::ZERO,
            current: Vec3::ONE,
        };

        assert_eq!(history.interpolated(-1.0), Vec3::ZERO);
        assert_eq!(history.interpolated(2.0), Vec3::ONE);
    }
}
