use bevy::prelude::*;

use crate::arena_barriers::ArenaBarrierDefinition;
use crate::arena_defs::{
    VENT_SPIRAL_REACTOR_BASE_HALF_EXTENT, VENT_SPIRAL_REACTOR_BASE_TOP_Y,
    VENT_SPIRAL_REACTOR_BASE_Y, VENT_SPIRAL_REACTOR_SCALE,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PropBarrierBehavior {
    Solid,
    OneWayTop,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum LocalPropFootprint {
    Circle { radius: f32 },
    Rectangle { half_extents: Vec2, yaw: f32 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LocalPropBarrier {
    center: Vec2,
    top_y: f32,
    footprint: LocalPropFootprint,
    behavior: PropBarrierBehavior,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WorldPropBarrier {
    pub definition: ArenaBarrierDefinition,
    pub behavior: PropBarrierBehavior,
}

impl LocalPropBarrier {
    const fn solid_rect(x: f32, z: f32, hx: f32, hz: f32, yaw: f32, top_y: f32) -> Self {
        Self::rectangle(x, z, hx, hz, yaw, top_y, PropBarrierBehavior::Solid)
    }

    const fn top_rect(x: f32, z: f32, hx: f32, hz: f32, yaw: f32, top_y: f32) -> Self {
        Self::rectangle(x, z, hx, hz, yaw, top_y, PropBarrierBehavior::OneWayTop)
    }

    const fn rectangle(
        x: f32,
        z: f32,
        hx: f32,
        hz: f32,
        yaw: f32,
        top_y: f32,
        behavior: PropBarrierBehavior,
    ) -> Self {
        Self {
            center: Vec2::new(x, z),
            top_y,
            footprint: LocalPropFootprint::Rectangle {
                half_extents: Vec2::new(hx, hz),
                yaw,
            },
            behavior,
        }
    }

    const fn solid_circle(x: f32, z: f32, radius: f32, top_y: f32) -> Self {
        Self {
            center: Vec2::new(x, z),
            top_y,
            footprint: LocalPropFootprint::Circle { radius },
            behavior: PropBarrierBehavior::Solid,
        }
    }

    pub fn to_world(self, position: Vec3, yaw: f32, scale: f32) -> WorldPropBarrier {
        let scale = scale.abs();
        let center = Vec2::new(position.x, position.z) + rotate(self.center * scale, yaw);
        let top_y = position.y + self.top_y * scale;
        let definition = match self.footprint {
            LocalPropFootprint::Circle { radius } => {
                ArenaBarrierDefinition::circle(center.x, center.y, radius * scale, top_y)
            }
            LocalPropFootprint::Rectangle {
                half_extents,
                yaw: local_yaw,
            } => ArenaBarrierDefinition::rectangle(
                center.x,
                center.y,
                half_extents.x * scale,
                half_extents.y * scale,
                yaw + local_yaw,
                top_y,
            ),
        };
        WorldPropBarrier {
            definition,
            behavior: self.behavior,
        }
    }
}

fn rotate(point: Vec2, yaw: f32) -> Vec2 {
    let cos = yaw.cos();
    let sin = yaw.sin();
    Vec2::new(cos * point.x - sin * point.y, sin * point.x + cos * point.y)
}

const NONE: &[LocalPropBarrier] = &[];
const STATUE: &[LocalPropBarrier] = &[LocalPropBarrier::solid_circle(0.0, 0.0, 0.28, 0.9)];
const TROPHY: &[LocalPropBarrier] = &[LocalPropBarrier::solid_circle(0.0, 0.0, 0.24, 0.55)];
const TREE_TRUNK: &[LocalPropBarrier] = &[LocalPropBarrier::solid_circle(0.0, 0.0, 0.14, 0.62)];
const ROCKS: &[LocalPropBarrier] = &[LocalPropBarrier::solid_rect(
    0.0, 0.0, 0.43, 0.42, 0.0, 0.415,
)];
const CONVEYOR: &[LocalPropBarrier] = &[LocalPropBarrier::solid_rect(
    0.0, 0.0, 0.46, 0.22, 0.0, 0.18,
)];
const REACTOR: &[LocalPropBarrier] = &[LocalPropBarrier::solid_circle(
    0.0,
    0.0,
    VENT_SPIRAL_REACTOR_BASE_HALF_EXTENT / VENT_SPIRAL_REACTOR_SCALE,
    (VENT_SPIRAL_REACTOR_BASE_TOP_Y - VENT_SPIRAL_REACTOR_BASE_Y) / VENT_SPIRAL_REACTOR_SCALE,
)];
const SPRING: &[LocalPropBarrier] = &[LocalPropBarrier::solid_circle(0.0, 0.0, 0.34, 0.24)];
const TARGET: &[LocalPropBarrier] = &[LocalPropBarrier::solid_rect(
    0.0, 0.0, 0.05, 0.17, 0.0, 0.17,
)];
const BURGER: &[LocalPropBarrier] = &[LocalPropBarrier::solid_circle(0.0, 0.0, 0.19, 0.32)];
const CAKE: &[LocalPropBarrier] = &[LocalPropBarrier::solid_circle(0.0, 0.0, 0.41, 0.273)];
const PIZZA: &[LocalPropBarrier] = &[LocalPropBarrier::solid_circle(0.0, 0.0, 0.42, 0.043)];
const WATERMELON: &[LocalPropBarrier] = &[LocalPropBarrier::solid_circle(0.0, 0.0, 0.14, 0.476)];
const STEW_POT: &[LocalPropBarrier] = &[LocalPropBarrier::solid_rect(
    0.0, 0.0, 0.33, 0.41, 0.0, 0.361,
)];
const CRATE: &[LocalPropBarrier] = &[LocalPropBarrier::solid_rect(0.0, 0.0, 0.25, 0.25, 0.0, 0.5)];
const HEDGE: &[LocalPropBarrier] = &[LocalPropBarrier::solid_rect(0.0, 0.35, 0.5, 0.15, 0.0, 0.4)];
const HEDGE_CORNER: &[LocalPropBarrier] = &[
    LocalPropBarrier::solid_rect(0.0, 0.35, 0.5, 0.15, 0.0, 0.4),
    LocalPropBarrier::solid_rect(0.35, 0.0, 0.15, 0.5, 0.0, 0.4),
];
const SNOWMAN: &[LocalPropBarrier] = &[LocalPropBarrier::solid_rect(
    0.0, 0.0, 0.55, 0.34, 0.0, 1.064,
)];
const SNOW_PILE: &[LocalPropBarrier] =
    &[LocalPropBarrier::solid_rect(0.0, 0.0, 0.49, 0.55, 0.0, 0.2)];
const CANNON: &[LocalPropBarrier] = &[LocalPropBarrier::solid_circle(0.0, 0.0, 0.3, 0.44)];
const CATAPULT: &[LocalPropBarrier] = &[LocalPropBarrier::solid_rect(
    0.0, -0.1, 0.3, 0.42, 0.0, 0.44,
)];
const BARREL: &[LocalPropBarrier] = &[LocalPropBarrier::solid_circle(0.0, 0.0, 0.259, 0.476)];
const BOMB: &[LocalPropBarrier] = &[LocalPropBarrier::solid_circle(0.0, 0.0, 0.247, 0.538)];
const CANNONBALL: &[LocalPropBarrier] = &[LocalPropBarrier::solid_circle(0.0, 0.0, 0.14, 0.14)];

const WOOD_STRUCTURE_HIGH: &[LocalPropBarrier] = &[
    LocalPropBarrier::solid_rect(-0.4, -0.32, 0.07, 0.07, 0.0, 0.78),
    LocalPropBarrier::solid_rect(0.4, -0.32, 0.07, 0.07, 0.0, 0.78),
    LocalPropBarrier::solid_rect(-0.4, 0.32, 0.07, 0.07, 0.0, 0.78),
    LocalPropBarrier::solid_rect(0.4, 0.32, 0.07, 0.07, 0.0, 0.78),
    LocalPropBarrier::top_rect(0.0, -0.32, 0.48, 0.08, 0.0, 1.0),
    LocalPropBarrier::top_rect(0.0, 0.32, 0.48, 0.08, 0.0, 1.0),
    LocalPropBarrier::top_rect(-0.4, 0.0, 0.07, 0.4, 0.0, 1.0),
    LocalPropBarrier::top_rect(0.4, 0.0, 0.07, 0.4, 0.0, 1.0),
];

const WOOD_STRUCTURE: &[LocalPropBarrier] = &[
    LocalPropBarrier::solid_rect(-0.34, -0.28, 0.07, 0.07, 0.0, 0.5),
    LocalPropBarrier::solid_rect(0.34, -0.28, 0.07, 0.07, 0.0, 0.5),
    LocalPropBarrier::solid_rect(-0.34, 0.28, 0.07, 0.07, 0.0, 0.5),
    LocalPropBarrier::solid_rect(0.34, 0.28, 0.07, 0.07, 0.0, 0.5),
    LocalPropBarrier::top_rect(0.0, -0.28, 0.41, 0.08, 0.0, 0.5),
    LocalPropBarrier::top_rect(0.0, 0.28, 0.41, 0.08, 0.0, 0.5),
    LocalPropBarrier::top_rect(-0.34, 0.0, 0.07, 0.35, 0.0, 0.5),
    LocalPropBarrier::top_rect(0.34, 0.0, 0.07, 0.35, 0.0, 0.5),
];

const SNOW_WOOD_STRUCTURE: &[LocalPropBarrier] = &[
    LocalPropBarrier::solid_rect(-0.4, -0.3, 0.07, 0.07, 0.0, 0.5),
    LocalPropBarrier::solid_rect(0.4, -0.3, 0.07, 0.07, 0.0, 0.5),
    LocalPropBarrier::solid_rect(-0.4, 0.3, 0.07, 0.07, 0.0, 0.5),
    LocalPropBarrier::solid_rect(0.4, 0.3, 0.07, 0.07, 0.0, 0.5),
    LocalPropBarrier::top_rect(0.0, -0.3, 0.48, 0.08, 0.0, 0.5),
    LocalPropBarrier::top_rect(0.0, 0.3, 0.48, 0.08, 0.0, 0.5),
    LocalPropBarrier::top_rect(-0.4, 0.0, 0.07, 0.38, 0.0, 0.5),
    LocalPropBarrier::top_rect(0.4, 0.0, 0.07, 0.38, 0.0, 0.5),
];

pub fn prop_collision_profile(asset: &str) -> &'static [LocalPropBarrier] {
    match asset {
        "statue.glb" => STATUE,
        "banner.glb" => NONE,
        "trophy.glb" => TROPHY,
        "tower/wood-structure-high.glb" => WOOD_STRUCTURE_HIGH,
        "tower/detail-tree-large.glb" => TREE_TRUNK,
        "tower/wood-structure.glb" => WOOD_STRUCTURE,
        "tower/detail-rocks-large.glb" => ROCKS,
        "platformer/conveyor-belt.glb" => CONVEYOR,
        "platformer/pipe.glb" => NONE,
        "tower/tower-round-crystals.glb" => REACTOR,
        "platformer/spring.glb" => SPRING,
        "blaster/target-large.glb" => TARGET,
        "food/burger-cheese-double.glb" => BURGER,
        "food/cake.glb" => CAKE,
        "food/pizza.glb" => PIZZA,
        "food/watermelon.glb" => WATERMELON,
        "food/pot-stew.glb" => STEW_POT,
        "platformer/crate.glb" => CRATE,
        "platformer/hedge.glb" => HEDGE,
        "platformer/hedge-corner.glb" => HEDGE_CORNER,
        "platformer/flowers-tall.glb" | "platformer/flowers.glb" => NONE,
        "platformer/tree.glb" => TREE_TRUNK,
        "platformer/tree-pine-snow.glb" | "platformer/tree-pine-snow-small.glb" => TREE_TRUNK,
        "holiday/snowman.glb" => SNOWMAN,
        "holiday/lantern.glb" => NONE,
        "tower/snow-wood-structure.glb" => SNOW_WOOD_STRUCTURE,
        "holiday/snow-pile.glb" => SNOW_PILE,
        "tower/snow-detail-rocks-large.glb" => ROCKS,
        "tower/weapon-cannon.glb" => CANNON,
        "tower/weapon-catapult.glb" => CATAPULT,
        "platformer/barrel.glb" => BARREL,
        "platformer/bomb.glb" => BOMB,
        "tower/weapon-ammo-cannonball.glb" => CANNONBALL,
        _ => panic!("arena prop asset {asset:?} needs an explicit collision profile"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arena_barriers::BarrierSupport;

    #[test]
    fn prop_transform_applies_scale_rotation_and_translation() {
        let world = LocalPropBarrier::solid_rect(1.0, 0.0, 0.5, 0.25, 0.0, 0.4).to_world(
            Vec3::new(3.0, 2.0, 4.0),
            std::f32::consts::FRAC_PI_2,
            2.0,
        );
        assert!((world.definition.center.x - 3.0).abs() < 0.001);
        assert!((world.definition.center.y - 6.0).abs() < 0.001);
        assert!((world.definition.top_y - 2.8).abs() < 0.001);
    }

    #[test]
    fn hedge_corner_does_not_fill_its_empty_quadrant() {
        let empty_point = Vec2::new(-0.35, -0.35);
        assert!(HEDGE_CORNER.iter().all(|barrier| {
            barrier
                .to_world(Vec3::ZERO, 0.0, 1.0)
                .definition
                .support_at(empty_point, 0.0)
                .is_none()
        }));
        assert_eq!(
            HEDGE_CORNER[0]
                .to_world(Vec3::ZERO, 0.0, 1.0)
                .definition
                .support_at(Vec2::new(0.3, 0.35), 0.0),
            Some(BarrierSupport::Firm)
        );
    }

    #[test]
    fn measured_flat_tops_do_not_exceed_visible_mesh_bounds() {
        for (profile, visible_top) in [
            (CAKE, 0.273),
            (PIZZA, 0.043),
            (HEDGE, 0.4),
            (SNOW_PILE, 0.2),
            (CATAPULT, 0.44),
            (CANNONBALL, 0.14),
        ] {
            assert!(profile.iter().all(|barrier| barrier.top_y <= visible_top));
        }
    }

    #[test]
    fn tree_profile_only_uses_the_trunk() {
        let trunk = TREE_TRUNK[0].to_world(Vec3::ZERO, 0.0, 1.0).definition;
        assert_eq!(
            trunk.support_at(Vec2::ZERO, 0.0),
            Some(BarrierSupport::Firm)
        );
        assert_eq!(trunk.support_at(Vec2::new(0.5, 0.0), 0.0), None);
    }

    #[test]
    fn reactor_profile_keeps_the_authored_visible_top() {
        let reactor = REACTOR[0].to_world(
            Vec3::new(0.0, VENT_SPIRAL_REACTOR_BASE_Y, 0.0),
            0.0,
            VENT_SPIRAL_REACTOR_SCALE,
        );
        assert!((reactor.definition.top_y - VENT_SPIRAL_REACTOR_BASE_TOP_Y).abs() < 0.001);
    }
}
