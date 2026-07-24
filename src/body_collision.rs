use bevy::prelude::*;

use crate::characters::CharacterBodyDef;
use crate::constants::{KENNEY_CUBE_PET_GROUND_OFFSET, KENNEY_CUBE_PET_SCALE};

const BODY_SEPARATION_EPSILON: f32 = 0.001;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FighterBodyBox {
    pub center: Vec3,
    pub half_extents: Vec3,
    pub right: Vec3,
    pub forward: Vec3,
}

impl FighterBodyBox {
    #[cfg(all(
        feature = "dev-hot-reload",
        not(feature = "shipping"),
        not(target_arch = "wasm32")
    ))]
    pub fn corners(self) -> [Vec3; 8] {
        let right = self.right * self.half_extents.x;
        let up = Vec3::Y * self.half_extents.y;
        let forward = self.forward * self.half_extents.z;
        [
            self.center - right - up - forward,
            self.center + right - up - forward,
            self.center + right - up + forward,
            self.center - right - up + forward,
            self.center - right + up - forward,
            self.center + right + up - forward,
            self.center + right + up + forward,
            self.center - right + up + forward,
        ]
    }
}

pub fn fighter_body_box(
    root_position: Vec3,
    facing: Vec3,
    body: CharacterBodyDef,
    size_multiplier: f32,
) -> FighterBodyBox {
    let scale = KENNEY_CUBE_PET_SCALE * size_multiplier.max(0.01);
    let min = Vec3::from_array(body.mesh_bounds.min);
    let max = Vec3::from_array(body.mesh_bounds.max);
    let local_center = (min + max) * 0.5 * scale;
    let half_extents = (max - min) * 0.5 * scale;
    let forward = normalized_planar_facing(facing);
    let right =
        crate::canonical_math::vec3_normalize_or_zero(Vec3::new(forward.z, 0.0, -forward.x));

    FighterBodyBox {
        center: root_position
            + right * local_center.x
            + Vec3::Y * (KENNEY_CUBE_PET_GROUND_OFFSET + local_center.y)
            + forward * local_center.z,
        half_extents,
        right,
        forward,
    }
}

pub fn body_box_separation(a: FighterBodyBox, b: FighterBodyBox) -> Option<Vec2> {
    if (a.center.y - b.center.y).abs() >= a.half_extents.y + b.half_extents.y {
        return None;
    }

    let axes = [
        body_axis_xz(a.right),
        body_axis_xz(a.forward),
        body_axis_xz(b.right),
        body_axis_xz(b.forward),
    ];
    let center_delta = body_center_xz(a) - body_center_xz(b);
    let mut smallest_overlap = f32::MAX;
    let mut smallest_axis = Vec2::X;

    for axis in axes {
        if crate::canonical_math::vec2_length_squared(axis) <= 0.0001 {
            continue;
        }
        let axis = crate::canonical_math::vec2_normalize_or_zero(axis);
        let distance = center_delta.dot(axis);
        let overlap =
            body_projection_radius(a, axis) + body_projection_radius(b, axis) - distance.abs();
        if overlap <= 0.0 {
            return None;
        }
        if overlap < smallest_overlap {
            smallest_overlap = overlap;
            smallest_axis = if distance >= 0.0 { axis } else { -axis };
        }
    }

    Some(smallest_axis * (smallest_overlap + BODY_SEPARATION_EPSILON))
}

pub fn body_box_landing_correction(
    upper: FighterBodyBox,
    lower: FighterBodyBox,
    max_penetration: f32,
) -> Option<f32> {
    if upper.center.y <= lower.center.y || !body_boxes_overlap_xz(upper, lower) {
        return None;
    }

    let upper_bottom = upper.center.y - upper.half_extents.y;
    let lower_top = lower.center.y + lower.half_extents.y;
    let correction = lower_top - upper_bottom;
    (correction >= 0.0 && correction <= max_penetration).then_some(correction)
}

fn body_boxes_overlap_xz(a: FighterBodyBox, b: FighterBodyBox) -> bool {
    let axes = [
        body_axis_xz(a.right),
        body_axis_xz(a.forward),
        body_axis_xz(b.right),
        body_axis_xz(b.forward),
    ];
    let center_delta = body_center_xz(a) - body_center_xz(b);
    axes.into_iter()
        .filter(|axis| crate::canonical_math::vec2_length_squared(*axis) > 0.0001)
        .all(|axis| {
            let axis = crate::canonical_math::vec2_normalize_or_zero(axis);
            center_delta.dot(axis).abs()
                < body_projection_radius(a, axis) + body_projection_radius(b, axis)
        })
}

pub fn sphere_body_box_contact(
    sphere_center: Vec3,
    sphere_radius: f32,
    body: FighterBodyBox,
) -> Option<Vec3> {
    let contact = closest_point_on_body_box(sphere_center, body);
    debug_assert!(sphere_radius >= 0.0);
    (crate::canonical_math::vec3_distance_squared(contact, sphere_center)
        <= sphere_radius * sphere_radius)
        .then_some(contact)
}

fn closest_point_on_body_box(point: Vec3, body: FighterBodyBox) -> Vec3 {
    let delta = point - body.center;
    body.center
        + body.right
            * delta
                .dot(body.right)
                .clamp(-body.half_extents.x, body.half_extents.x)
        + Vec3::Y * delta.y.clamp(-body.half_extents.y, body.half_extents.y)
        + body.forward
            * delta
                .dot(body.forward)
                .clamp(-body.half_extents.z, body.half_extents.z)
}

fn body_projection_radius(body: FighterBodyBox, axis: Vec2) -> f32 {
    body.half_extents.x * axis.dot(body_axis_xz(body.right)).abs()
        + body.half_extents.z * axis.dot(body_axis_xz(body.forward)).abs()
}

fn body_axis_xz(axis: Vec3) -> Vec2 {
    crate::canonical_math::vec2_normalize_or_zero(Vec2::new(axis.x, axis.z))
}

fn body_center_xz(body: FighterBodyBox) -> Vec2 {
    Vec2::new(body.center.x, body.center.z)
}

fn normalized_planar_facing(facing: Vec3) -> Vec3 {
    let planar = Vec3::new(facing.x, 0.0, facing.z);
    if crate::canonical_math::vec3_length_squared(planar) > 0.0001 {
        crate::canonical_math::vec3_normalize_or_zero(planar)
    } else {
        Vec3::Z
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::characters::{CharacterKind, character_body_profile};

    fn assert_close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 0.002,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn fighter_body_box_uses_scaled_character_mesh_bounds() {
        let cat = fighter_body_box(
            Vec3::ZERO,
            Vec3::Z,
            character_body_profile(CharacterKind::Cat),
            1.0,
        );
        let fox = fighter_body_box(
            Vec3::ZERO,
            Vec3::Z,
            character_body_profile(CharacterKind::Fox),
            1.0,
        );

        assert_close(cat.half_extents.x, 0.525);
        assert_close(cat.half_extents.z, 0.557);
        assert_close(cat.center.y, 0.714);
        assert!(fox.half_extents.z > cat.half_extents.z);
    }

    #[test]
    fn body_box_separation_resolves_visible_footprints() {
        let body = character_body_profile(CharacterKind::Cat);
        let a = fighter_body_box(Vec3::ZERO, Vec3::Z, body, 1.0);
        let b = fighter_body_box(Vec3::new(0.35, 0.0, 0.0), Vec3::Z, body, 1.0);

        let correction = body_box_separation(a, b).unwrap();
        let resolved_a = fighter_body_box(
            Vec3::new(correction.x * 0.5, 0.0, correction.y * 0.5),
            Vec3::Z,
            body,
            1.0,
        );
        let resolved_b = fighter_body_box(
            Vec3::new(0.35 - correction.x * 0.5, 0.0, -correction.y * 0.5),
            Vec3::Z,
            body,
            1.0,
        );

        assert!(body_box_separation(resolved_a, resolved_b).is_none());
    }

    #[test]
    fn body_box_separation_ignores_clear_vertical_stacks() {
        let body = character_body_profile(CharacterKind::Cat);
        let grounded = fighter_body_box(Vec3::ZERO, Vec3::Z, body, 1.0);
        let airborne = fighter_body_box(Vec3::new(0.0, 2.0, 0.0), Vec3::Z, body, 1.0);

        assert!(body_box_separation(grounded, airborne).is_none());
    }

    #[test]
    fn descending_body_can_land_on_another_body() {
        let body = character_body_profile(CharacterKind::Cat);
        let lower = fighter_body_box(Vec3::ZERO, Vec3::Z, body, 1.0);
        let upper = fighter_body_box(Vec3::new(0.0, 1.3, 0.0), Vec3::Z, body, 1.0);

        let correction = body_box_landing_correction(upper, lower, 0.5).unwrap();
        assert!(correction > 0.0);
        assert!(body_box_landing_correction(lower, upper, 0.5).is_none());
    }

    #[test]
    fn sphere_body_box_contact_hits_faces_and_misses_outside() {
        let body = fighter_body_box(
            Vec3::ZERO,
            Vec3::Z,
            character_body_profile(CharacterKind::Cat),
            1.0,
        );
        let front_face_z = body.center.z + body.half_extents.z;

        assert!(
            sphere_body_box_contact(
                Vec3::new(0.0, body.center.y, front_face_z + 0.2),
                0.21,
                body
            )
            .is_some()
        );
        assert!(
            sphere_body_box_contact(
                Vec3::new(0.0, body.center.y, front_face_z + 0.24),
                0.21,
                body
            )
            .is_none()
        );
        assert!(
            sphere_body_box_contact(
                Vec3::new(body.half_extents.x + 0.2, body.center.y, front_face_z + 0.2),
                0.21,
                body,
            )
            .is_none()
        );
    }

    #[test]
    fn item_giant_scale_expands_body_hurtbox() {
        let normal = fighter_body_box(
            Vec3::ZERO,
            Vec3::Z,
            character_body_profile(CharacterKind::Panda),
            1.0,
        );
        let giant = fighter_body_box(
            Vec3::ZERO,
            Vec3::Z,
            character_body_profile(CharacterKind::Panda),
            1.5,
        );

        assert!(giant.half_extents.x > normal.half_extents.x);
        assert!(giant.half_extents.y > normal.half_extents.y);
        assert!(giant.half_extents.z > normal.half_extents.z);
    }
}
