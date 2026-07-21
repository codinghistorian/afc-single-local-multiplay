use bevy::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BarrierFootprint {
    #[allow(dead_code)]
    Circle {
        radius: f32,
    },
    Rectangle {
        half_extents: Vec2,
        yaw: f32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarrierSupport {
    Firm,
    Grace,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ArenaBarrierDefinition {
    pub center: Vec2,
    pub half_extents: Vec2,
    pub top_y: f32,
    pub footprint: BarrierFootprint,
}

impl ArenaBarrierDefinition {
    pub const fn new(x: f32, z: f32, hx: f32, hz: f32, top_y: f32) -> Self {
        Self::rectangle(x, z, hx, hz, 0.0, top_y)
    }

    pub const fn rectangle(x: f32, z: f32, hx: f32, hz: f32, yaw: f32, top_y: f32) -> Self {
        let half_extents = Vec2::new(hx, hz);
        Self {
            center: Vec2::new(x, z),
            half_extents,
            top_y,
            footprint: BarrierFootprint::Rectangle { half_extents, yaw },
        }
    }

    #[allow(dead_code)]
    pub const fn circle(x: f32, z: f32, radius: f32, top_y: f32) -> Self {
        Self {
            center: Vec2::new(x, z),
            half_extents: Vec2::splat(radius),
            top_y,
            footprint: BarrierFootprint::Circle { radius },
        }
    }

    pub fn block_mesh(&self, height: f32) -> Mesh {
        match self.footprint {
            BarrierFootprint::Circle { radius } => Mesh::from(Cylinder::new(radius, height)),
            BarrierFootprint::Rectangle { half_extents, .. } => Mesh::from(Cuboid::new(
                half_extents.x * 2.0,
                height,
                half_extents.y * 2.0,
            )),
        }
    }

    pub fn block_transform(&self, height: f32) -> Transform {
        let yaw = match self.footprint {
            BarrierFootprint::Circle { .. } => 0.0,
            BarrierFootprint::Rectangle { yaw, .. } => yaw,
        };
        Transform::from_xyz(self.center.x, self.top_y - height * 0.5, self.center.y)
            .with_rotation(Quat::from_rotation_y(yaw))
    }

    pub fn support_at(&self, point: Vec2, ledge_grace: f32) -> Option<BarrierSupport> {
        match self.footprint {
            BarrierFootprint::Circle { radius } => {
                let distance = (point - self.center).length();
                if distance <= radius {
                    Some(BarrierSupport::Firm)
                } else if distance <= radius + ledge_grace {
                    Some(BarrierSupport::Grace)
                } else {
                    None
                }
            }
            BarrierFootprint::Rectangle { half_extents, yaw } => {
                let local = rotate_into_local(point - self.center, yaw).abs();
                if local.x <= half_extents.x && local.y <= half_extents.y {
                    Some(BarrierSupport::Firm)
                } else if local.x <= half_extents.x + ledge_grace
                    && local.y <= half_extents.y + ledge_grace
                {
                    Some(BarrierSupport::Grace)
                } else {
                    None
                }
            }
        }
    }

    pub fn resolve_side_collision(
        &self,
        position: Vec3,
        fighter_radius: f32,
        landing_tolerance: f32,
    ) -> Vec3 {
        if position.y >= self.top_y - landing_tolerance {
            return position;
        }

        match self.footprint {
            BarrierFootprint::Circle { radius } => {
                let offset = Vec2::new(position.x, position.z) - self.center;
                let expanded_radius = radius + fighter_radius;
                if offset.length() >= expanded_radius {
                    return position;
                }
                let resolved = self.center + offset.normalize_or(Vec2::X) * expanded_radius;
                Vec3::new(resolved.x, position.y, resolved.y)
            }
            BarrierFootprint::Rectangle { half_extents, yaw } => {
                let offset = Vec2::new(position.x, position.z) - self.center;
                let local = rotate_into_local(offset, yaw);
                let expanded = half_extents + Vec2::splat(fighter_radius);
                if local.x.abs() >= expanded.x || local.y.abs() >= expanded.y {
                    return position;
                }

                let push_x = expanded.x - local.x.abs();
                let push_z = expanded.y - local.y.abs();
                let resolved_local = if push_x < push_z {
                    Vec2::new(expanded.x * nonzero_sign(local.x), local.y)
                } else {
                    Vec2::new(local.x, expanded.y * nonzero_sign(local.y))
                };
                let resolved = self.center + rotate_from_local(resolved_local, yaw);
                Vec3::new(resolved.x, position.y, resolved.y)
            }
        }
    }
}

fn rotate_into_local(offset: Vec2, yaw: f32) -> Vec2 {
    let cos = yaw.cos();
    let sin = yaw.sin();
    Vec2::new(
        cos * offset.x + sin * offset.y,
        -sin * offset.x + cos * offset.y,
    )
}

fn rotate_from_local(offset: Vec2, yaw: f32) -> Vec2 {
    let cos = yaw.cos();
    let sin = yaw.sin();
    Vec2::new(
        cos * offset.x - sin * offset.y,
        sin * offset.x + cos * offset.y,
    )
}

fn nonzero_sign(value: f32) -> f32 {
    if value < 0.0 { -1.0 } else { 1.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotated_rectangle_support_matches_its_rendered_axes() {
        let barrier = ArenaBarrierDefinition::rectangle(0.0, 0.0, 1.0, 0.5, 0.5, 1.0);
        let inside = rotate_from_local(Vec2::new(0.9, 0.4), 0.5);
        let outside = rotate_from_local(Vec2::new(1.1, 0.4), 0.5);

        assert_eq!(barrier.support_at(inside, 0.0), Some(BarrierSupport::Firm));
        assert_eq!(barrier.support_at(outside, 0.0), None);
    }

    #[test]
    fn landing_height_clears_the_visible_side_wall() {
        let barrier = ArenaBarrierDefinition::new(0.0, 0.0, 1.0, 1.0, 0.65);
        let approach = Vec3::new(0.0, 0.0, -1.2);
        assert_ne!(
            barrier.resolve_side_collision(approach, 0.4, 0.08),
            approach
        );

        let landing = Vec3::new(approach.x, barrier.top_y - 0.08, approach.z);
        assert_eq!(barrier.resolve_side_collision(landing, 0.4, 0.08), landing);
    }
}
