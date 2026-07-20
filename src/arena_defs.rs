use bevy::prelude::*;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::constants::{ARENA_TOP_Y, CAMERA_BASE_OFFSET, RINGOUT_RADIUS, RINGOUT_Y};
use crate::items::ItemKind;

#[derive(Clone, Copy)]
pub struct PlatformDefinition {
    pub center: Vec2,
    pub half_extents: Vec2,
    pub top_y: f32,
}

impl PlatformDefinition {
    pub const fn new(x: f32, z: f32, hx: f32, hz: f32, top_y: f32) -> Self {
        Self {
            center: Vec2::new(x, z),
            half_extents: Vec2::new(hx, hz),
            top_y,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ArenaGroundShape {
    Circle {
        center: Vec2,
        radius: f32,
        top_y: f32,
    },
    Rectangle {
        center: Vec2,
        half_extents: Vec2,
        yaw: f32,
        top_y: f32,
    },
}

impl ArenaGroundShape {
    pub const fn circle(x: f32, z: f32, radius: f32, top_y: f32) -> Self {
        Self::Circle {
            center: Vec2::new(x, z),
            radius,
            top_y,
        }
    }

    pub const fn rectangle(x: f32, z: f32, half_x: f32, half_z: f32, yaw: f32, top_y: f32) -> Self {
        Self::Rectangle {
            center: Vec2::new(x, z),
            half_extents: Vec2::new(half_x, half_z),
            yaw,
            top_y,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArenaVisualTheme {
    Crown,
    Causeway,
    Terrace,
    Industrial,
    Reactor,
    Toybox,
    Market,
    Garden,
    Snow,
    Powder,
}

#[derive(Clone, Copy)]
pub struct ItemAnchor {
    pub kind: ItemKind,
    pub position: Vec3,
    pub phase: f32,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArenaHazardKind {
    PulseVent,
    SnareField,
    BumperNode,
}

#[derive(Clone, Copy)]
pub struct ArenaHazardDefinition {
    pub kind: ArenaHazardKind,
    pub center: Vec3,
    pub radius: f32,
    pub pulse_seconds: f32,
    pub phase: f32,
}

#[derive(Clone, Copy)]
pub struct ArenaBackgroundDefinition {
    pub asset_path: &'static str,
    pub image_size: Vec2,
    pub world_height: f32,
    pub position: Vec3,
}

pub struct ArenaDefinition {
    pub name: &'static str,
    pub spawn_points: [Vec3; 4],
    pub item_anchors: &'static [ItemAnchor],
    pub ground_shapes: &'static [ArenaGroundShape],
    pub platforms: &'static [PlatformDefinition],
    pub ringout_radius: f32,
    pub ringout_y: f32,
    pub camera_offset: Vec3,
    pub hazards: &'static [ArenaHazardDefinition],
    pub background: ArenaBackgroundDefinition,
    pub visual_theme: ArenaVisualTheme,
}

const ANIME_SKY_BACKGROUND: ArenaBackgroundDefinition = ArenaBackgroundDefinition {
    asset_path: "backgrounds/beautiful_sky_anime.png",
    image_size: Vec2::new(1536.0, 1024.0),
    world_height: 300.0,
    position: Vec3::new(0.0, 24.0, -24.0),
};

const CROWN_GROUND: &[ArenaGroundShape] = &[ArenaGroundShape::circle(
    0.0,
    0.0,
    crate::constants::ARENA_RADIUS,
    ARENA_TOP_Y,
)];

const SPLIT_GROUND: &[ArenaGroundShape] = &[
    ArenaGroundShape::rectangle(-4.7, 0.0, 3.0, 6.5, 0.0, ARENA_TOP_Y),
    ArenaGroundShape::rectangle(4.7, 0.0, 3.0, 6.5, 0.0, ARENA_TOP_Y),
    ArenaGroundShape::rectangle(0.0, 0.0, 1.75, 6.5, 0.0, ARENA_TOP_Y - 0.12),
    ArenaGroundShape::rectangle(0.0, 4.7, 2.0, 1.15, 0.0, ARENA_TOP_Y + 0.04),
    ArenaGroundShape::rectangle(0.0, -4.7, 2.0, 1.15, 0.0, ARENA_TOP_Y + 0.04),
];

const SUNSTONE_GROUND: &[ArenaGroundShape] = &[
    ArenaGroundShape::circle(0.0, 0.0, 8.15, ARENA_TOP_Y - 0.16),
    ArenaGroundShape::circle(0.0, 0.0, 3.25, ARENA_TOP_Y + 0.18),
    ArenaGroundShape::circle(-5.9, 3.8, 2.0, ARENA_TOP_Y + 0.08),
    ArenaGroundShape::circle(5.9, -3.8, 2.0, ARENA_TOP_Y + 0.08),
    ArenaGroundShape::circle(-5.8, -4.7, 1.65, ARENA_TOP_Y),
    ArenaGroundShape::circle(5.8, 4.7, 1.65, ARENA_TOP_Y),
    ArenaGroundShape::rectangle(-3.25, 2.15, 2.2, 0.62, -0.55, ARENA_TOP_Y - 0.06),
    ArenaGroundShape::rectangle(3.25, -2.15, 2.2, 0.62, -0.55, ARENA_TOP_Y - 0.06),
];

const CRANK_GROUND: &[ArenaGroundShape] = &[
    ArenaGroundShape::rectangle(0.0, 0.0, 7.6, 2.35, 0.0, ARENA_TOP_Y),
    ArenaGroundShape::rectangle(0.0, 0.0, 2.35, 7.6, 0.0, ARENA_TOP_Y),
    ArenaGroundShape::rectangle(-5.8, -5.8, 1.8, 1.8, 0.0, ARENA_TOP_Y + 0.12),
    ArenaGroundShape::rectangle(5.8, -5.8, 1.8, 1.8, 0.0, ARENA_TOP_Y + 0.12),
    ArenaGroundShape::rectangle(-5.8, 5.8, 1.8, 1.8, 0.0, ARENA_TOP_Y + 0.12),
    ArenaGroundShape::rectangle(5.8, 5.8, 1.8, 1.8, 0.0, ARENA_TOP_Y + 0.12),
];

const VENT_SPIRAL_GROUND: &[ArenaGroundShape] = &[
    ArenaGroundShape::circle(0.0, 0.0, 2.45, ARENA_TOP_Y),
    ArenaGroundShape::rectangle(3.5, 0.0, 2.5, 1.35, 0.0, ARENA_TOP_Y),
    ArenaGroundShape::rectangle(5.0, 2.5, 2.25, 1.25, 0.78, ARENA_TOP_Y + 0.08),
    ArenaGroundShape::rectangle(3.0, 5.0, 2.35, 1.25, 0.0, ARENA_TOP_Y + 0.16),
    ArenaGroundShape::rectangle(-0.3, 5.5, 2.25, 1.25, 0.0, ARENA_TOP_Y + 0.24),
    ArenaGroundShape::rectangle(-3.7, 4.2, 2.4, 1.25, -0.7, ARENA_TOP_Y + 0.3),
    ArenaGroundShape::rectangle(-5.2, 1.2, 2.25, 1.25, 0.0, ARENA_TOP_Y + 0.36),
    ArenaGroundShape::rectangle(-4.0, -2.2, 2.45, 1.25, 0.7, ARENA_TOP_Y + 0.42),
    ArenaGroundShape::rectangle(-1.2, -4.4, 2.25, 1.25, 0.0, ARENA_TOP_Y + 0.48),
    ArenaGroundShape::rectangle(2.4, -4.5, 2.45, 1.25, 0.0, ARENA_TOP_Y + 0.54),
];

const BUMPER_GROUND: &[ArenaGroundShape] = &[
    ArenaGroundShape::rectangle(0.0, 0.0, 4.25, 8.3, 0.0, ARENA_TOP_Y),
    ArenaGroundShape::circle(0.0, 7.7, 4.25, ARENA_TOP_Y),
    ArenaGroundShape::circle(0.0, -7.7, 4.25, ARENA_TOP_Y),
];

const FEAST_MARKET_GROUND: &[ArenaGroundShape] = &[
    ArenaGroundShape::rectangle(0.0, 0.0, 5.0, 4.4, 0.0, ARENA_TOP_Y),
    ArenaGroundShape::rectangle(-5.4, 2.8, 2.5, 2.1, 0.0, ARENA_TOP_Y),
    ArenaGroundShape::rectangle(5.4, -2.8, 2.5, 2.1, 0.0, ARENA_TOP_Y),
    ArenaGroundShape::rectangle(-2.8, -5.5, 2.1, 2.5, 0.0, ARENA_TOP_Y),
    ArenaGroundShape::rectangle(2.8, 5.5, 2.1, 2.5, 0.0, ARENA_TOP_Y),
];

const SNARE_GARDEN_GROUND: &[ArenaGroundShape] = &[
    ArenaGroundShape::circle(0.0, 0.0, 2.8, ARENA_TOP_Y),
    ArenaGroundShape::circle(-5.1, 0.0, 2.7, ARENA_TOP_Y),
    ArenaGroundShape::circle(5.1, 0.0, 2.7, ARENA_TOP_Y),
    ArenaGroundShape::circle(0.0, -5.1, 2.7, ARENA_TOP_Y),
    ArenaGroundShape::circle(0.0, 5.1, 2.7, ARENA_TOP_Y),
    ArenaGroundShape::rectangle(-2.6, 0.0, 1.8, 0.85, 0.0, ARENA_TOP_Y),
    ArenaGroundShape::rectangle(2.6, 0.0, 1.8, 0.85, 0.0, ARENA_TOP_Y),
    ArenaGroundShape::rectangle(0.0, -2.6, 0.85, 1.8, 0.0, ARENA_TOP_Y),
    ArenaGroundShape::rectangle(0.0, 2.6, 0.85, 1.8, 0.0, ARENA_TOP_Y),
];

const SKY_STEPS_GROUND: &[ArenaGroundShape] = &[
    ArenaGroundShape::rectangle(-6.0, -4.8, 2.25, 1.8, 0.0, ARENA_TOP_Y),
    ArenaGroundShape::rectangle(-3.0, -2.4, 2.2, 1.7, 0.35, ARENA_TOP_Y + 0.18),
    ArenaGroundShape::rectangle(0.0, 0.0, 2.3, 1.8, 0.35, ARENA_TOP_Y + 0.38),
    ArenaGroundShape::rectangle(3.0, 2.4, 2.2, 1.7, 0.35, ARENA_TOP_Y + 0.58),
    ArenaGroundShape::rectangle(6.0, 4.8, 2.25, 1.8, 0.0, ARENA_TOP_Y + 0.8),
    ArenaGroundShape::rectangle(-5.6, 4.7, 1.8, 1.6, 0.0, ARENA_TOP_Y + 0.28),
    ArenaGroundShape::rectangle(5.6, -4.7, 1.8, 1.6, 0.0, ARENA_TOP_Y + 0.28),
];

const POWDER_KEG_GROUND: &[ArenaGroundShape] = &[
    ArenaGroundShape::rectangle(0.0, 0.0, 5.8, 7.6, 0.0, ARENA_TOP_Y),
    ArenaGroundShape::circle(0.0, 6.9, 5.8, ARENA_TOP_Y),
    ArenaGroundShape::circle(0.0, -6.9, 5.8, ARENA_TOP_Y),
    ArenaGroundShape::rectangle(-7.0, 0.0, 1.45, 3.2, 0.0, ARENA_TOP_Y + 0.12),
    ArenaGroundShape::rectangle(7.0, 0.0, 1.45, 3.2, 0.0, ARENA_TOP_Y + 0.12),
];

const CROWN_PLATFORMS: &[PlatformDefinition] = &[
    PlatformDefinition::new(0.0, 9.65, 3.9, 1.65, ARENA_TOP_Y - 0.05),
    PlatformDefinition::new(0.0, -9.65, 4.4, 1.65, ARENA_TOP_Y - 0.05),
    PlatformDefinition::new(-9.55, 0.0, 1.55, 4.2, ARENA_TOP_Y - 0.05),
    PlatformDefinition::new(9.55, 0.0, 1.55, 4.2, ARENA_TOP_Y - 0.05),
];

const CROWN_ITEMS: &[ItemAnchor] = &[
    ItemAnchor {
        kind: ItemKind::Apple,
        position: Vec3::new(-5.35, ARENA_TOP_Y + 0.48, 0.0),
        phase: 0.0,
    },
    ItemAnchor {
        kind: ItemKind::WineWhite,
        position: Vec3::new(0.0, ARENA_TOP_Y + 0.48, 5.35),
        phase: 1.7,
    },
    ItemAnchor {
        kind: ItemKind::Turkey,
        position: Vec3::new(5.35, ARENA_TOP_Y + 0.5, 0.0),
        phase: 3.4,
    },
    ItemAnchor {
        kind: ItemKind::Steamer,
        position: Vec3::new(0.0, ARENA_TOP_Y + 0.46, -5.35),
        phase: 5.1,
    },
    ItemAnchor {
        kind: ItemKind::Barrel,
        position: Vec3::new(-3.8, ARENA_TOP_Y + 0.44, -4.2),
        phase: 2.2,
    },
    ItemAnchor {
        kind: ItemKind::CupCoffee,
        position: Vec3::new(3.8, ARENA_TOP_Y + 0.5, 4.2),
        phase: 4.2,
    },
    ItemAnchor {
        kind: ItemKind::Mushroom,
        position: Vec3::new(-6.2, ARENA_TOP_Y + 0.56, 3.0),
        phase: 5.8,
    },
    ItemAnchor {
        kind: ItemKind::Steamer,
        position: Vec3::new(6.2, ARENA_TOP_Y + 0.5, -3.0),
        phase: 0.9,
    },
];

const SPLIT_PLATFORMS: &[PlatformDefinition] = &[
    PlatformDefinition::new(-4.8, 0.0, 2.1, 5.8, ARENA_TOP_Y - 0.04),
    PlatformDefinition::new(4.8, 0.0, 2.1, 5.8, ARENA_TOP_Y - 0.04),
    PlatformDefinition::new(0.0, 6.8, 2.8, 1.2, ARENA_TOP_Y + 0.24),
    PlatformDefinition::new(0.0, -6.8, 2.8, 1.2, ARENA_TOP_Y + 0.24),
];

const SPLIT_ITEMS: &[ItemAnchor] = &[
    ItemAnchor {
        kind: ItemKind::Barrel,
        position: Vec3::new(-4.8, ARENA_TOP_Y + 0.44, 0.0),
        phase: 0.4,
    },
    ItemAnchor {
        kind: ItemKind::Mushroom,
        position: Vec3::new(4.8, ARENA_TOP_Y + 0.5, 0.0),
        phase: 2.0,
    },
    ItemAnchor {
        kind: ItemKind::Steamer,
        position: Vec3::new(0.0, ARENA_TOP_Y + 0.46, 6.8),
        phase: 3.6,
    },
    ItemAnchor {
        kind: ItemKind::CupCoffee,
        position: Vec3::new(0.0, ARENA_TOP_Y + 0.5, -6.8),
        phase: 5.2,
    },
];

const SUNSTONE_PLATFORMS: &[PlatformDefinition] = &[
    PlatformDefinition::new(0.0, 0.0, 3.2, 2.4, ARENA_TOP_Y + 0.18),
    PlatformDefinition::new(-6.4, 3.8, 1.8, 1.2, ARENA_TOP_Y + 0.52),
    PlatformDefinition::new(6.4, -3.8, 1.8, 1.2, ARENA_TOP_Y + 0.52),
    PlatformDefinition::new(0.0, -8.6, 4.2, 1.0, ARENA_TOP_Y - 0.08),
];

const SUNSTONE_ITEMS: &[ItemAnchor] = &[
    ItemAnchor {
        kind: ItemKind::CupCoffee,
        position: Vec3::new(0.0, ARENA_TOP_Y + 0.68, 0.0),
        phase: 0.6,
    },
    ItemAnchor {
        kind: ItemKind::Barrel,
        position: Vec3::new(-6.4, ARENA_TOP_Y + 0.96, 3.8),
        phase: 2.1,
    },
    ItemAnchor {
        kind: ItemKind::Turkey,
        position: Vec3::new(6.4, ARENA_TOP_Y + 0.96, -3.8),
        phase: 3.4,
    },
    ItemAnchor {
        kind: ItemKind::Apple,
        position: Vec3::new(-5.8, ARENA_TOP_Y + 0.48, -4.7),
        phase: 4.8,
    },
    ItemAnchor {
        kind: ItemKind::Steamer,
        position: Vec3::new(5.8, ARENA_TOP_Y + 0.46, 4.7),
        phase: 5.6,
    },
];

const CRANK_PLATFORMS: &[PlatformDefinition] = &[
    PlatformDefinition::new(0.0, 0.0, 1.5, 6.6, ARENA_TOP_Y + 0.12),
    PlatformDefinition::new(-5.8, -5.8, 1.55, 1.55, ARENA_TOP_Y + 0.38),
    PlatformDefinition::new(5.8, -5.8, 1.55, 1.55, ARENA_TOP_Y + 0.38),
    PlatformDefinition::new(-5.8, 5.8, 1.55, 1.55, ARENA_TOP_Y + 0.38),
    PlatformDefinition::new(5.8, 5.8, 1.55, 1.55, ARENA_TOP_Y + 0.38),
];

const CRANK_ITEMS: &[ItemAnchor] = &[
    ItemAnchor {
        kind: ItemKind::Turkey,
        position: Vec3::new(0.0, ARENA_TOP_Y + 0.62, 3.4),
        phase: 0.3,
    },
    ItemAnchor {
        kind: ItemKind::Mushroom,
        position: Vec3::new(0.0, ARENA_TOP_Y + 0.62, -3.4),
        phase: 1.5,
    },
    ItemAnchor {
        kind: ItemKind::Barrel,
        position: Vec3::new(-5.8, ARENA_TOP_Y + 0.82, 5.8),
        phase: 2.7,
    },
    ItemAnchor {
        kind: ItemKind::Steamer,
        position: Vec3::new(5.8, ARENA_TOP_Y + 0.8, -5.8),
        phase: 4.0,
    },
    ItemAnchor {
        kind: ItemKind::WineWhite,
        position: Vec3::new(5.8, ARENA_TOP_Y + 0.82, 5.8),
        phase: 5.4,
    },
];

const VENT_SPIRAL_PLATFORMS: &[PlatformDefinition] = &[
    PlatformDefinition::new(6.85, 2.15, 2.0, 1.15, ARENA_TOP_Y + 0.16),
    PlatformDefinition::new(2.15, 6.85, 1.15, 2.0, ARENA_TOP_Y + 0.28),
    PlatformDefinition::new(-6.85, -2.15, 2.0, 1.15, ARENA_TOP_Y + 0.16),
    PlatformDefinition::new(-2.15, -6.85, 1.15, 2.0, ARENA_TOP_Y + 0.28),
];

const VENT_SPIRAL_ITEMS: &[ItemAnchor] = &[
    ItemAnchor {
        kind: ItemKind::CupCoffee,
        position: Vec3::new(-4.6, ARENA_TOP_Y + 0.5, 3.8),
        phase: 0.8,
    },
    ItemAnchor {
        kind: ItemKind::Mushroom,
        position: Vec3::new(4.6, ARENA_TOP_Y + 0.5, -3.8),
        phase: 2.4,
    },
    ItemAnchor {
        kind: ItemKind::Steamer,
        position: Vec3::new(6.85, ARENA_TOP_Y + 0.6, 2.15),
        phase: 3.6,
    },
    ItemAnchor {
        kind: ItemKind::Turkey,
        position: Vec3::new(-2.15, ARENA_TOP_Y + 0.72, -6.85),
        phase: 5.0,
    },
];

const BUMPER_ALLEY_PLATFORMS: &[PlatformDefinition] = &[
    PlatformDefinition::new(0.0, 9.35, 1.65, 1.95, ARENA_TOP_Y - 0.06),
    PlatformDefinition::new(0.0, -9.35, 1.65, 1.95, ARENA_TOP_Y - 0.06),
    PlatformDefinition::new(-9.35, 0.0, 1.95, 1.65, ARENA_TOP_Y - 0.06),
    PlatformDefinition::new(9.35, 0.0, 1.95, 1.65, ARENA_TOP_Y - 0.06),
];

const BUMPER_ALLEY_ITEMS: &[ItemAnchor] = &[
    ItemAnchor {
        kind: ItemKind::Barrel,
        position: Vec3::new(0.0, ARENA_TOP_Y + 0.56, 5.9),
        phase: 0.5,
    },
    ItemAnchor {
        kind: ItemKind::Barrel,
        position: Vec3::new(0.0, ARENA_TOP_Y + 0.56, -5.9),
        phase: 2.0,
    },
    ItemAnchor {
        kind: ItemKind::WineWhite,
        position: Vec3::new(-3.4, ARENA_TOP_Y + 0.48, 0.0),
        phase: 3.5,
    },
    ItemAnchor {
        kind: ItemKind::Apple,
        position: Vec3::new(3.4, ARENA_TOP_Y + 0.44, 0.0),
        phase: 5.0,
    },
];

const FEAST_MARKET_PLATFORMS: &[PlatformDefinition] = &[
    PlatformDefinition::new(-6.25, 2.75, 1.8, 1.1, ARENA_TOP_Y - 0.04),
    PlatformDefinition::new(6.25, -2.75, 1.8, 1.1, ARENA_TOP_Y - 0.04),
    PlatformDefinition::new(-2.75, -6.25, 1.1, 1.8, ARENA_TOP_Y - 0.04),
    PlatformDefinition::new(2.75, 6.25, 1.1, 1.8, ARENA_TOP_Y - 0.04),
];

const FEAST_MARKET_ITEMS: &[ItemAnchor] = &[
    ItemAnchor {
        kind: ItemKind::Apple,
        position: Vec3::new(-4.7, ARENA_TOP_Y + 0.44, 0.0),
        phase: 0.0,
    },
    ItemAnchor {
        kind: ItemKind::Turkey,
        position: Vec3::new(4.7, ARENA_TOP_Y + 0.5, 0.0),
        phase: 0.9,
    },
    ItemAnchor {
        kind: ItemKind::WineWhite,
        position: Vec3::new(0.0, ARENA_TOP_Y + 0.48, 4.0),
        phase: 1.8,
    },
    ItemAnchor {
        kind: ItemKind::CupCoffee,
        position: Vec3::new(0.0, ARENA_TOP_Y + 0.5, -4.0),
        phase: 2.7,
    },
    ItemAnchor {
        kind: ItemKind::Mushroom,
        position: Vec3::new(-5.8, ARENA_TOP_Y + 0.5, 4.8),
        phase: 3.6,
    },
    ItemAnchor {
        kind: ItemKind::Barrel,
        position: Vec3::new(5.8, ARENA_TOP_Y + 0.56, -4.8),
        phase: 4.5,
    },
    ItemAnchor {
        kind: ItemKind::Steamer,
        position: Vec3::new(3.0, ARENA_TOP_Y + 0.46, 5.5),
        phase: 5.4,
    },
    ItemAnchor {
        kind: ItemKind::Apple,
        position: Vec3::new(-3.0, ARENA_TOP_Y + 0.44, -5.5),
        phase: 6.3,
    },
];

const SNARE_GARDEN_PLATFORMS: &[PlatformDefinition] = &[
    PlatformDefinition::new(0.0, 8.9, 3.15, 1.35, ARENA_TOP_Y - 0.05),
    PlatformDefinition::new(0.0, -8.9, 3.15, 1.35, ARENA_TOP_Y - 0.05),
    PlatformDefinition::new(-8.9, 0.0, 1.35, 3.15, ARENA_TOP_Y - 0.05),
    PlatformDefinition::new(8.9, 0.0, 1.35, 3.15, ARENA_TOP_Y - 0.05),
];

const SNARE_GARDEN_ITEMS: &[ItemAnchor] = &[
    ItemAnchor {
        kind: ItemKind::CupCoffee,
        position: Vec3::new(-6.0, ARENA_TOP_Y + 0.5, 0.0),
        phase: 0.2,
    },
    ItemAnchor {
        kind: ItemKind::CupCoffee,
        position: Vec3::new(6.0, ARENA_TOP_Y + 0.5, 0.0),
        phase: 1.8,
    },
    ItemAnchor {
        kind: ItemKind::WineWhite,
        position: Vec3::new(0.0, ARENA_TOP_Y + 0.48, 6.0),
        phase: 3.4,
    },
    ItemAnchor {
        kind: ItemKind::Mushroom,
        position: Vec3::new(0.0, ARENA_TOP_Y + 0.5, -6.0),
        phase: 5.0,
    },
    ItemAnchor {
        kind: ItemKind::Steamer,
        position: Vec3::new(0.0, ARENA_TOP_Y + 0.46, 0.0),
        phase: 6.2,
    },
];

const SKY_STEPS_PLATFORMS: &[PlatformDefinition] = &[
    PlatformDefinition::new(-5.7, -4.6, 1.55, 1.3, ARENA_TOP_Y + 0.22),
    PlatformDefinition::new(-1.9, -1.55, 1.75, 1.35, ARENA_TOP_Y + 0.42),
    PlatformDefinition::new(1.9, 1.55, 1.75, 1.35, ARENA_TOP_Y + 0.62),
    PlatformDefinition::new(5.7, 4.6, 1.55, 1.3, ARENA_TOP_Y + 0.82),
    PlatformDefinition::new(-5.7, 4.6, 1.55, 1.3, ARENA_TOP_Y + 0.34),
    PlatformDefinition::new(5.7, -4.6, 1.55, 1.3, ARENA_TOP_Y + 0.34),
];

const SKY_STEPS_ITEMS: &[ItemAnchor] = &[
    ItemAnchor {
        kind: ItemKind::Turkey,
        position: Vec3::new(5.7, ARENA_TOP_Y + 1.26, 4.6),
        phase: 0.7,
    },
    ItemAnchor {
        kind: ItemKind::Mushroom,
        position: Vec3::new(-5.7, ARENA_TOP_Y + 0.78, 4.6),
        phase: 2.2,
    },
    ItemAnchor {
        kind: ItemKind::Barrel,
        position: Vec3::new(-5.7, ARENA_TOP_Y + 0.66, -4.6),
        phase: 3.7,
    },
    ItemAnchor {
        kind: ItemKind::Steamer,
        position: Vec3::new(5.7, ARENA_TOP_Y + 0.78, -4.6),
        phase: 5.2,
    },
    ItemAnchor {
        kind: ItemKind::WineWhite,
        position: Vec3::new(0.0, ARENA_TOP_Y + 0.48, 0.0),
        phase: 6.0,
    },
];

const POWDER_KEG_PLATFORMS: &[PlatformDefinition] = &[
    PlatformDefinition::new(-7.4, 0.0, 1.25, 3.4, ARENA_TOP_Y - 0.04),
    PlatformDefinition::new(7.4, 0.0, 1.25, 3.4, ARENA_TOP_Y - 0.04),
    PlatformDefinition::new(0.0, 7.4, 3.4, 1.25, ARENA_TOP_Y - 0.04),
    PlatformDefinition::new(0.0, -7.4, 3.4, 1.25, ARENA_TOP_Y - 0.04),
];

const POWDER_KEG_ITEMS: &[ItemAnchor] = &[
    ItemAnchor {
        kind: ItemKind::Steamer,
        position: Vec3::new(-4.8, ARENA_TOP_Y + 0.46, 0.0),
        phase: 0.1,
    },
    ItemAnchor {
        kind: ItemKind::Steamer,
        position: Vec3::new(4.8, ARENA_TOP_Y + 0.46, 0.0),
        phase: 1.3,
    },
    ItemAnchor {
        kind: ItemKind::Steamer,
        position: Vec3::new(0.0, ARENA_TOP_Y + 0.46, -4.8),
        phase: 2.5,
    },
    ItemAnchor {
        kind: ItemKind::Barrel,
        position: Vec3::new(0.0, ARENA_TOP_Y + 0.56, 4.8),
        phase: 3.7,
    },
    ItemAnchor {
        kind: ItemKind::Apple,
        position: Vec3::new(-4.8, ARENA_TOP_Y + 0.44, 6.6),
        phase: 4.8,
    },
    ItemAnchor {
        kind: ItemKind::CupCoffee,
        position: Vec3::new(4.8, ARENA_TOP_Y + 0.5, -6.6),
        phase: 5.8,
    },
];

const CROWN_HAZARDS: &[ArenaHazardDefinition] = &[];

const SPLIT_HAZARDS: &[ArenaHazardDefinition] = &[ArenaHazardDefinition {
    kind: ArenaHazardKind::SnareField,
    center: Vec3::new(0.0, ARENA_TOP_Y + 0.05, -4.2),
    radius: 1.65,
    pulse_seconds: 3.1,
    phase: 0.0,
}];

const SUNSTONE_HAZARDS: &[ArenaHazardDefinition] = &[ArenaHazardDefinition {
    kind: ArenaHazardKind::SnareField,
    center: Vec3::new(0.0, ARENA_TOP_Y + 0.05, 1.8),
    radius: 1.8,
    pulse_seconds: 3.3,
    phase: 0.0,
}];

const CRANK_HAZARDS: &[ArenaHazardDefinition] = &[
    ArenaHazardDefinition {
        kind: ArenaHazardKind::BumperNode,
        center: Vec3::new(-3.1, ARENA_TOP_Y + 0.05, 0.0),
        radius: 0.95,
        pulse_seconds: 2.1,
        phase: 0.0,
    },
    ArenaHazardDefinition {
        kind: ArenaHazardKind::BumperNode,
        center: Vec3::new(3.1, ARENA_TOP_Y + 0.05, 0.0),
        radius: 0.95,
        pulse_seconds: 2.1,
        phase: 1.05,
    },
];

const VENT_SPIRAL_HAZARDS: &[ArenaHazardDefinition] = &[
    ArenaHazardDefinition {
        kind: ArenaHazardKind::PulseVent,
        center: Vec3::new(0.0, ARENA_TOP_Y + 0.06, 0.0),
        radius: 1.05,
        pulse_seconds: 2.45,
        phase: 0.0,
    },
    ArenaHazardDefinition {
        kind: ArenaHazardKind::PulseVent,
        center: Vec3::new(-3.7, ARENA_TOP_Y + 0.06, 3.7),
        radius: 0.9,
        pulse_seconds: 3.0,
        phase: 0.8,
    },
    ArenaHazardDefinition {
        kind: ArenaHazardKind::PulseVent,
        center: Vec3::new(3.7, ARENA_TOP_Y + 0.06, -3.7),
        radius: 0.9,
        pulse_seconds: 3.0,
        phase: 1.8,
    },
    ArenaHazardDefinition {
        kind: ArenaHazardKind::SnareField,
        center: Vec3::new(4.5, ARENA_TOP_Y + 0.05, 1.0),
        radius: 1.25,
        pulse_seconds: 4.2,
        phase: 2.1,
    },
];

const BUMPER_ALLEY_HAZARDS: &[ArenaHazardDefinition] = &[
    ArenaHazardDefinition {
        kind: ArenaHazardKind::BumperNode,
        center: Vec3::new(0.0, ARENA_TOP_Y + 0.05, 4.25),
        radius: 0.85,
        pulse_seconds: 1.9,
        phase: 0.0,
    },
    ArenaHazardDefinition {
        kind: ArenaHazardKind::BumperNode,
        center: Vec3::new(0.0, ARENA_TOP_Y + 0.05, 0.0),
        radius: 0.85,
        pulse_seconds: 1.9,
        phase: 0.65,
    },
    ArenaHazardDefinition {
        kind: ArenaHazardKind::BumperNode,
        center: Vec3::new(0.0, ARENA_TOP_Y + 0.05, -4.25),
        radius: 0.85,
        pulse_seconds: 1.9,
        phase: 1.3,
    },
];

const FEAST_MARKET_HAZARDS: &[ArenaHazardDefinition] = &[];

const SNARE_GARDEN_HAZARDS: &[ArenaHazardDefinition] = &[
    ArenaHazardDefinition {
        kind: ArenaHazardKind::SnareField,
        center: Vec3::new(-3.0, ARENA_TOP_Y + 0.05, 0.0),
        radius: 1.55,
        pulse_seconds: 3.8,
        phase: 0.0,
    },
    ArenaHazardDefinition {
        kind: ArenaHazardKind::SnareField,
        center: Vec3::new(3.0, ARENA_TOP_Y + 0.05, 0.0),
        radius: 1.55,
        pulse_seconds: 3.8,
        phase: 1.9,
    },
    ArenaHazardDefinition {
        kind: ArenaHazardKind::SnareField,
        center: Vec3::new(0.0, ARENA_TOP_Y + 0.05, 3.0),
        radius: 1.4,
        pulse_seconds: 4.4,
        phase: 1.1,
    },
    ArenaHazardDefinition {
        kind: ArenaHazardKind::SnareField,
        center: Vec3::new(0.0, ARENA_TOP_Y + 0.05, -3.0),
        radius: 1.4,
        pulse_seconds: 4.4,
        phase: 3.3,
    },
];

const SKY_STEPS_HAZARDS: &[ArenaHazardDefinition] = &[
    ArenaHazardDefinition {
        kind: ArenaHazardKind::PulseVent,
        center: Vec3::new(-5.6, ARENA_TOP_Y + 0.34, 4.7),
        radius: 0.95,
        pulse_seconds: 3.2,
        phase: 0.0,
    },
    ArenaHazardDefinition {
        kind: ArenaHazardKind::PulseVent,
        center: Vec3::new(5.6, ARENA_TOP_Y + 0.34, -4.7),
        radius: 0.95,
        pulse_seconds: 3.2,
        phase: 1.6,
    },
];

const POWDER_KEG_HAZARDS: &[ArenaHazardDefinition] = &[
    ArenaHazardDefinition {
        kind: ArenaHazardKind::PulseVent,
        center: Vec3::new(0.0, ARENA_TOP_Y + 0.06, 0.0),
        radius: 1.15,
        pulse_seconds: 2.8,
        phase: 0.0,
    },
    ArenaHazardDefinition {
        kind: ArenaHazardKind::BumperNode,
        center: Vec3::new(-4.4, ARENA_TOP_Y + 0.05, 4.4),
        radius: 0.85,
        pulse_seconds: 2.3,
        phase: 0.8,
    },
    ArenaHazardDefinition {
        kind: ArenaHazardKind::BumperNode,
        center: Vec3::new(4.4, ARENA_TOP_Y + 0.05, -4.4),
        radius: 0.85,
        pulse_seconds: 2.3,
        phase: 1.9,
    },
];

const ARENAS: &[ArenaDefinition] = &[
    ArenaDefinition {
        name: "Crown Ring",
        spawn_points: [
            Vec3::new(-3.6, ARENA_TOP_Y, 2.8),
            Vec3::new(3.6, ARENA_TOP_Y, 2.8),
            Vec3::new(-3.6, ARENA_TOP_Y, -2.8),
            Vec3::new(3.6, ARENA_TOP_Y, -2.8),
        ],
        item_anchors: CROWN_ITEMS,
        ground_shapes: CROWN_GROUND,
        platforms: CROWN_PLATFORMS,
        ringout_radius: RINGOUT_RADIUS,
        ringout_y: RINGOUT_Y,
        camera_offset: CAMERA_BASE_OFFSET,
        hazards: CROWN_HAZARDS,
        background: ANIME_SKY_BACKGROUND,
        visual_theme: ArenaVisualTheme::Crown,
    },
    ArenaDefinition {
        name: "Split Causeway",
        spawn_points: [
            Vec3::new(-5.1, ARENA_TOP_Y, 2.8),
            Vec3::new(5.1, ARENA_TOP_Y, 2.8),
            Vec3::new(-5.1, ARENA_TOP_Y, -2.8),
            Vec3::new(5.1, ARENA_TOP_Y, -2.8),
        ],
        item_anchors: SPLIT_ITEMS,
        ground_shapes: SPLIT_GROUND,
        platforms: SPLIT_PLATFORMS,
        ringout_radius: RINGOUT_RADIUS + 1.0,
        ringout_y: RINGOUT_Y,
        camera_offset: Vec3::new(0.0, 13.0, 15.2),
        hazards: SPLIT_HAZARDS,
        background: ANIME_SKY_BACKGROUND,
        visual_theme: ArenaVisualTheme::Causeway,
    },
    ArenaDefinition {
        name: "Sunstone Steps",
        spawn_points: [
            Vec3::new(-4.8, ARENA_TOP_Y, 4.0),
            Vec3::new(4.8, ARENA_TOP_Y, -4.0),
            Vec3::new(-4.8, ARENA_TOP_Y, -4.0),
            Vec3::new(4.8, ARENA_TOP_Y, 4.0),
        ],
        item_anchors: SUNSTONE_ITEMS,
        ground_shapes: SUNSTONE_GROUND,
        platforms: SUNSTONE_PLATFORMS,
        ringout_radius: RINGOUT_RADIUS + 0.6,
        ringout_y: RINGOUT_Y - 0.25,
        camera_offset: Vec3::new(0.0, 13.6, 15.6),
        hazards: SUNSTONE_HAZARDS,
        background: ANIME_SKY_BACKGROUND,
        visual_theme: ArenaVisualTheme::Terrace,
    },
    ArenaDefinition {
        name: "Crank Yard",
        spawn_points: [
            Vec3::new(-6.1, ARENA_TOP_Y, 0.0),
            Vec3::new(6.1, ARENA_TOP_Y, 0.0),
            Vec3::new(0.0, ARENA_TOP_Y, -5.6),
            Vec3::new(0.0, ARENA_TOP_Y, 5.6),
        ],
        item_anchors: CRANK_ITEMS,
        ground_shapes: CRANK_GROUND,
        platforms: CRANK_PLATFORMS,
        ringout_radius: RINGOUT_RADIUS + 0.35,
        ringout_y: RINGOUT_Y,
        camera_offset: Vec3::new(0.0, 12.8, 14.8),
        hazards: CRANK_HAZARDS,
        background: ANIME_SKY_BACKGROUND,
        visual_theme: ArenaVisualTheme::Industrial,
    },
    ArenaDefinition {
        name: "Vent Spiral",
        spawn_points: [
            Vec3::new(-4.5, ARENA_TOP_Y, 1.8),
            Vec3::new(4.5, ARENA_TOP_Y, -1.0),
            Vec3::new(-1.8, ARENA_TOP_Y, -4.5),
            Vec3::new(1.8, ARENA_TOP_Y, 4.5),
        ],
        item_anchors: VENT_SPIRAL_ITEMS,
        ground_shapes: VENT_SPIRAL_GROUND,
        platforms: VENT_SPIRAL_PLATFORMS,
        ringout_radius: RINGOUT_RADIUS + 0.45,
        ringout_y: RINGOUT_Y,
        camera_offset: Vec3::new(0.0, 13.2, 15.4),
        hazards: VENT_SPIRAL_HAZARDS,
        background: ANIME_SKY_BACKGROUND,
        visual_theme: ArenaVisualTheme::Reactor,
    },
    ArenaDefinition {
        name: "Bumper Alley",
        spawn_points: [
            Vec3::new(-3.7, ARENA_TOP_Y, 5.0),
            Vec3::new(3.7, ARENA_TOP_Y, -5.0),
            Vec3::new(-3.7, ARENA_TOP_Y, -5.0),
            Vec3::new(3.7, ARENA_TOP_Y, 5.0),
        ],
        item_anchors: BUMPER_ALLEY_ITEMS,
        ground_shapes: BUMPER_GROUND,
        platforms: BUMPER_ALLEY_PLATFORMS,
        ringout_radius: RINGOUT_RADIUS + 0.1,
        ringout_y: RINGOUT_Y,
        camera_offset: Vec3::new(0.0, 13.4, 15.8),
        hazards: BUMPER_ALLEY_HAZARDS,
        background: ANIME_SKY_BACKGROUND,
        visual_theme: ArenaVisualTheme::Toybox,
    },
    ArenaDefinition {
        name: "Feast Market",
        spawn_points: [
            Vec3::new(-4.0, ARENA_TOP_Y, 3.2),
            Vec3::new(4.0, ARENA_TOP_Y, -3.2),
            Vec3::new(-4.0, ARENA_TOP_Y, -3.2),
            Vec3::new(4.0, ARENA_TOP_Y, 3.2),
        ],
        item_anchors: FEAST_MARKET_ITEMS,
        ground_shapes: FEAST_MARKET_GROUND,
        platforms: FEAST_MARKET_PLATFORMS,
        ringout_radius: RINGOUT_RADIUS + 0.75,
        ringout_y: RINGOUT_Y,
        camera_offset: Vec3::new(0.0, 13.0, 15.2),
        hazards: FEAST_MARKET_HAZARDS,
        background: ANIME_SKY_BACKGROUND,
        visual_theme: ArenaVisualTheme::Market,
    },
    ArenaDefinition {
        name: "Snare Garden",
        spawn_points: [
            Vec3::new(-5.4, ARENA_TOP_Y, 1.6),
            Vec3::new(5.4, ARENA_TOP_Y, -1.6),
            Vec3::new(-1.6, ARENA_TOP_Y, -5.4),
            Vec3::new(1.6, ARENA_TOP_Y, 5.4),
        ],
        item_anchors: SNARE_GARDEN_ITEMS,
        ground_shapes: SNARE_GARDEN_GROUND,
        platforms: SNARE_GARDEN_PLATFORMS,
        ringout_radius: RINGOUT_RADIUS + 0.55,
        ringout_y: RINGOUT_Y,
        camera_offset: Vec3::new(0.0, 13.3, 15.4),
        hazards: SNARE_GARDEN_HAZARDS,
        background: ANIME_SKY_BACKGROUND,
        visual_theme: ArenaVisualTheme::Garden,
    },
    ArenaDefinition {
        name: "Sky Steps",
        spawn_points: [
            Vec3::new(-6.0, ARENA_TOP_Y, -4.8),
            Vec3::new(6.0, ARENA_TOP_Y + 0.8, 4.8),
            Vec3::new(-3.0, ARENA_TOP_Y + 0.18, -2.4),
            Vec3::new(3.0, ARENA_TOP_Y + 0.58, 2.4),
        ],
        item_anchors: SKY_STEPS_ITEMS,
        ground_shapes: SKY_STEPS_GROUND,
        platforms: SKY_STEPS_PLATFORMS,
        ringout_radius: RINGOUT_RADIUS + 0.85,
        ringout_y: RINGOUT_Y - 0.45,
        camera_offset: Vec3::new(0.0, 14.0, 16.2),
        hazards: SKY_STEPS_HAZARDS,
        background: ANIME_SKY_BACKGROUND,
        visual_theme: ArenaVisualTheme::Snow,
    },
    ArenaDefinition {
        name: "Powder Keg Court",
        spawn_points: [
            Vec3::new(-4.6, ARENA_TOP_Y, 3.4),
            Vec3::new(4.6, ARENA_TOP_Y, -3.4),
            Vec3::new(-4.6, ARENA_TOP_Y, -3.4),
            Vec3::new(4.6, ARENA_TOP_Y, 3.4),
        ],
        item_anchors: POWDER_KEG_ITEMS,
        ground_shapes: POWDER_KEG_GROUND,
        platforms: POWDER_KEG_PLATFORMS,
        ringout_radius: RINGOUT_RADIUS + 0.25,
        ringout_y: RINGOUT_Y,
        camera_offset: Vec3::new(0.0, 13.2, 15.6),
        hazards: POWDER_KEG_HAZARDS,
        background: ANIME_SKY_BACKGROUND,
        visual_theme: ArenaVisualTheme::Powder,
    },
];

static ACTIVE_ARENA_INDEX: AtomicUsize = AtomicUsize::new(0);

pub fn arena_definitions() -> &'static [ArenaDefinition] {
    ARENAS
}

pub fn arena_definition(index: usize) -> &'static ArenaDefinition {
    &ARENAS[index.min(ARENAS.len() - 1)]
}

pub fn active_arena_index() -> usize {
    ACTIVE_ARENA_INDEX
        .load(Ordering::Relaxed)
        .min(ARENAS.len() - 1)
}

pub fn set_active_arena_index(index: usize) {
    ACTIVE_ARENA_INDEX.store(index.min(ARENAS.len() - 1), Ordering::Relaxed);
}

pub fn active_arena_definition() -> &'static ArenaDefinition {
    arena_definition(active_arena_index())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arena_definitions_cover_current_stage_variety() {
        let arenas = arena_definitions();
        assert!(arenas.len() >= 10);
        assert_eq!(arenas[0].spawn_points.len(), 4);
        assert!(!arenas[0].item_anchors.is_empty());
        assert!(!arenas[1].hazards.is_empty());
        assert!(arenas[1].platforms[2].top_y > ARENA_TOP_Y);
        assert_eq!(arenas[2].name, "Sunstone Steps");
        assert_eq!(arenas[3].name, "Crank Yard");
        assert_eq!(arenas[4].name, "Vent Spiral");
        assert_eq!(arenas[5].name, "Bumper Alley");
        assert_eq!(arenas[6].name, "Feast Market");
        assert_eq!(arenas[7].name, "Snare Garden");
        assert_eq!(arenas[8].name, "Sky Steps");
        assert_eq!(arenas[9].name, "Powder Keg Court");
        assert!(arenas[2].ringout_y < RINGOUT_Y);
        assert!(arenas[3].hazards.len() >= 2);
        assert!(arenas[4].hazards.iter().any(|hazard| hazard.phase > 0.0));
        assert!(arenas[6].hazards.is_empty());
        assert!(
            arenas[8]
                .platforms
                .iter()
                .any(|platform| platform.top_y > ARENA_TOP_Y + 0.7)
        );
        assert!(
            arenas[9]
                .item_anchors
                .iter()
                .filter(|anchor| anchor.kind == ItemKind::Steamer)
                .count()
                >= 3
        );
    }

    #[test]
    fn arena_definition_clamps_selection_index() {
        assert_eq!(arena_definition(0).name, "Crown Ring");
        assert_eq!(arena_definition(1).name, "Split Causeway");
        assert_eq!(arena_definition(usize::MAX).name, "Powder Keg Court");
    }

    #[test]
    fn dry_redesigns_support_their_former_water_channels() {
        let split = arena_definition(1);
        for (x, z) in [(0.0, 0.0), (0.0, 2.5), (0.0, -2.5)] {
            assert!(
                crate::arena::ground_support_for_arena_with_radius(split, x, z, 0.0)
                    .height()
                    .is_some()
            );
        }

        let sunstone = arena_definition(2);
        for (x, z) in [(0.0, 5.5), (4.0, 2.5), (-4.0, -2.5)] {
            assert!(
                crate::arena::ground_support_for_arena_with_radius(sunstone, x, z, 0.0)
                    .height()
                    .is_some()
            );
        }
    }

    #[test]
    fn crown_extension_platforms_touch_main_arena() {
        let arena_radius = crate::constants::ARENA_RADIUS;
        let north = CROWN_PLATFORMS[0];
        let south = CROWN_PLATFORMS[1];
        let west = CROWN_PLATFORMS[2];
        let east = CROWN_PLATFORMS[3];

        assert!(north.center.y - north.half_extents.y <= arena_radius);
        assert!(south.center.y + south.half_extents.y >= -arena_radius);
        assert!(west.center.x + west.half_extents.x >= -arena_radius);
        assert!(east.center.x - east.half_extents.x <= arena_radius);
    }
}
