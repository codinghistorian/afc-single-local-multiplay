use bevy::prelude::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg(test)]
use crate::constants::CAMERA_BASE_OFFSET;
use crate::constants::{ARENA_TOP_Y, RINGOUT_RADIUS, RINGOUT_Y};
use crate::items::ItemKind;

pub use crate::arena_barriers::ArenaBarrierDefinition as PlatformDefinition;

pub const TRAINING_GROUND_ARENA_INDEX: usize = 10;
pub const TRAINING_GROUND_BACKGROUND_PATH: &str = "backgrounds/menu/map_select/arena0.png";

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
    Training,
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
    Campfire,
    SawBlade,
}

#[derive(Clone, Copy)]
pub struct ArenaHazardDefinition {
    pub kind: ArenaHazardKind,
    pub center: Vec3,
    pub radius: f32,
    pub pulse_seconds: f32,
    pub phase: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ArenaPipePairDefinition {
    pub endpoints: [Vec2; 2],
    pub top_y: f32,
    pub collider_radius: f32,
    pub trigger_radius: f32,
}

#[derive(Clone, Copy)]
pub struct ArenaBackgroundDefinition {
    pub asset_path: &'static str,
    pub image_size: Vec2,
    pub world_height: f32,
    pub distance: f32,
    pub gameplay_visible: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ArenaLightingProfile {
    pub ambient_color: Color,
    pub ambient_brightness: f32,
    pub directional_color: Color,
    pub directional_illuminance: f32,
    pub directional_position: Vec3,
    pub point_color: Color,
    pub point_intensity: f32,
    pub point_range: f32,
    pub point_position: Vec3,
}

pub struct ArenaDefinition {
    pub name: &'static str,
    pub spawn_points: [Vec3; 4],
    pub item_anchors: &'static [ItemAnchor],
    pub ground_shapes: &'static [ArenaGroundShape],
    pub platforms: &'static [PlatformDefinition],
    pub pipe_pair: Option<ArenaPipePairDefinition>,
    pub ringout_radius: f32,
    pub ringout_y: f32,
    pub camera_offset: Vec3,
    pub hazards: &'static [ArenaHazardDefinition],
    pub background: ArenaBackgroundDefinition,
    pub visual_theme: ArenaVisualTheme,
}

impl ArenaDefinition {
    pub fn gameplay_platforms(&self) -> impl Iterator<Item = &PlatformDefinition> {
        self.platforms.iter()
    }
}

const fn arena_background(asset_path: &'static str) -> ArenaBackgroundDefinition {
    ArenaBackgroundDefinition {
        asset_path,
        image_size: Vec2::new(1536.0, 1024.0),
        world_height: 52.0,
        distance: 52.0,
        gameplay_visible: true,
    }
}

const fn arena_background_with_size(
    asset_path: &'static str,
    image_size: Vec2,
) -> ArenaBackgroundDefinition {
    ArenaBackgroundDefinition {
        asset_path,
        image_size,
        world_height: 52.0,
        distance: 52.0,
        gameplay_visible: false,
    }
}

const CROWN_RING_BACKGROUND: ArenaBackgroundDefinition = ArenaBackgroundDefinition {
    asset_path: "backgrounds/crown_ring.png",
    image_size: Vec2::new(1448.0, 1086.0),
    // Crown Ring is substantially wider than the original arenas. Overscan its
    // 4:3 wallpaper so 16:9 gameplay windows never expose the plane's edges.
    world_height: 84.0,
    distance: 52.0,
    gameplay_visible: true,
};
const SPLIT_CAUSEWAY_BACKGROUND: ArenaBackgroundDefinition =
    arena_background("backgrounds/split_causeway.png");
const SUNSTONE_STEPS_BACKGROUND: ArenaBackgroundDefinition =
    arena_background("backgrounds/sunstone_steps.png");
const CRANK_YARD_BACKGROUND: ArenaBackgroundDefinition =
    arena_background("backgrounds/crank_yard.png");
const VENT_SPIRAL_BACKGROUND: ArenaBackgroundDefinition =
    arena_background("backgrounds/vent_spiral.png");
const BUMPER_ALLEY_BACKGROUND: ArenaBackgroundDefinition =
    arena_background("backgrounds/bumper_alley.png");
const FEAST_MARKET_BACKGROUND: ArenaBackgroundDefinition =
    arena_background("backgrounds/feast_market.png");
const SNARE_GARDEN_BACKGROUND: ArenaBackgroundDefinition =
    arena_background("backgrounds/snare_garden.png");
const SKY_STEPS_BACKGROUND: ArenaBackgroundDefinition =
    arena_background("backgrounds/sky_steps.png");
const POWDER_KEG_COURT_BACKGROUND: ArenaBackgroundDefinition =
    arena_background("backgrounds/powder_keg_court.png");
const TRAINING_GROUND_BACKGROUND: ArenaBackgroundDefinition =
    arena_background_with_size(TRAINING_GROUND_BACKGROUND_PATH, Vec2::new(1254.0, 1254.0));

// Reference-court coordinates use +Z for the camera-near side. The main court,
// front apron, and mirrored U-shaped wings are all floor-level support. The two
// rectangular holes inside the wings are intentionally absent.
const CROWN_GROUND: &[ArenaGroundShape] = &[
    ArenaGroundShape::rectangle(0.0, 0.25, 7.5, 6.75, 0.0, ARENA_TOP_Y),
    ArenaGroundShape::rectangle(0.0, 9.1, 5.2, 2.1, 0.0, ARENA_TOP_Y),
    ArenaGroundShape::rectangle(-8.25, 1.4, 0.75, 4.9, 0.0, ARENA_TOP_Y),
    ArenaGroundShape::rectangle(8.25, 1.4, 0.75, 4.9, 0.0, ARENA_TOP_Y),
    ArenaGroundShape::rectangle(-11.65, 1.4, 0.75, 4.9, 0.0, ARENA_TOP_Y),
    ArenaGroundShape::rectangle(11.65, 1.4, 0.75, 4.9, 0.0, ARENA_TOP_Y),
    ArenaGroundShape::rectangle(-9.95, -2.6, 0.95, 0.9, 0.0, ARENA_TOP_Y),
    ArenaGroundShape::rectangle(9.95, -2.6, 0.95, 0.9, 0.0, ARENA_TOP_Y),
    ArenaGroundShape::rectangle(-9.95, 5.4, 0.95, 0.9, 0.0, ARENA_TOP_Y),
    ArenaGroundShape::rectangle(9.95, 5.4, 0.95, 0.9, 0.0, ARENA_TOP_Y),
];

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

const VENT_SPIRAL_TIER_STEP: f32 = 0.65;
const VENT_SPIRAL_TIER_1_Y: f32 = ARENA_TOP_Y + VENT_SPIRAL_TIER_STEP;
const VENT_SPIRAL_TIER_2_Y: f32 = ARENA_TOP_Y + VENT_SPIRAL_TIER_STEP * 2.0;
const VENT_SPIRAL_TIER_3_Y: f32 = ARENA_TOP_Y + VENT_SPIRAL_TIER_STEP * 3.0;
pub const VENT_SPIRAL_REACTOR_SCALE: f32 = 3.1;
pub const VENT_SPIRAL_REACTOR_YAW: f32 = std::f32::consts::PI * 0.25;
pub const VENT_SPIRAL_REACTOR_BASE_Y: f32 = ARENA_TOP_Y + 0.02;
const VENT_SPIRAL_REACTOR_LOCAL_BASE_HALF_EXTENT: f32 = 0.5;
const VENT_SPIRAL_REACTOR_LOCAL_BASE_HEIGHT: f32 = 0.21;
pub(crate) const VENT_SPIRAL_REACTOR_BASE_HALF_EXTENT: f32 =
    VENT_SPIRAL_REACTOR_LOCAL_BASE_HALF_EXTENT * VENT_SPIRAL_REACTOR_SCALE;
pub(crate) const VENT_SPIRAL_REACTOR_BASE_TOP_Y: f32 =
    VENT_SPIRAL_REACTOR_BASE_Y + VENT_SPIRAL_REACTOR_LOCAL_BASE_HEIGHT * VENT_SPIRAL_REACTOR_SCALE;

const VENT_SPIRAL_GROUND: &[ArenaGroundShape] = &[
    ArenaGroundShape::circle(0.0, 0.0, 2.75, ARENA_TOP_Y),
    ArenaGroundShape::rectangle(4.0, 0.0, 2.2, 1.5, 0.0, ARENA_TOP_Y),
    ArenaGroundShape::rectangle(5.1, 2.35, 1.55, 1.55, 0.0, ARENA_TOP_Y),
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

const TRAINING_GROUND_GROUND: &[ArenaGroundShape] = &[ArenaGroundShape::rectangle(
    0.0,
    0.0,
    9.0,
    9.0,
    0.0,
    ARENA_TOP_Y,
)];

const CROWN_PLATFORMS: &[PlatformDefinition] = &[
    PlatformDefinition::new(0.0, -6.6, 5.2, 0.1, ARENA_TOP_Y + 0.07),
    PlatformDefinition::new(0.0, -6.8, 5.2, 0.1, ARENA_TOP_Y + 0.14),
    PlatformDefinition::new(0.0, -7.0, 5.2, 0.1, ARENA_TOP_Y + 0.21),
    PlatformDefinition::new(0.0, -7.2, 5.2, 0.1, ARENA_TOP_Y + 0.28),
    PlatformDefinition::new(0.0, -8.85, 5.2, 1.55, ARENA_TOP_Y + 0.28),
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
    PlatformDefinition::new(0.0, -8.6, 4.2, 1.0, ARENA_TOP_Y - 0.08),
    PlatformDefinition::new(0.0, 8.6, 4.2, 1.0, ARENA_TOP_Y - 0.08),
];

const SUNSTONE_ITEMS: &[ItemAnchor] = &[
    ItemAnchor {
        kind: ItemKind::CupCoffee,
        position: Vec3::new(0.0, ARENA_TOP_Y + 0.42, -8.6),
        phase: 0.6,
    },
    ItemAnchor {
        kind: ItemKind::Barrel,
        position: Vec3::new(-3.6, ARENA_TOP_Y + 0.28, 1.0),
        phase: 2.1,
    },
    ItemAnchor {
        kind: ItemKind::Turkey,
        position: Vec3::new(3.6, ARENA_TOP_Y + 0.28, -1.0),
        phase: 3.4,
    },
    ItemAnchor {
        kind: ItemKind::Apple,
        position: Vec3::new(-3.2, ARENA_TOP_Y + 0.32, -5.8),
        phase: 4.8,
    },
    ItemAnchor {
        kind: ItemKind::Steamer,
        position: Vec3::new(0.0, ARENA_TOP_Y + 0.38, 8.6),
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

const CRANK_PIPE_MODEL_HEIGHT: f32 = 0.564_285_76;
const CRANK_PIPE_MODEL_HALF_WIDTH: f32 = 0.68;
pub const CRANK_PIPE_VISUAL_SCALE: f32 = 1.5;
const CRANK_PIPE_TOP_Y: f32 = ARENA_TOP_Y + CRANK_PIPE_MODEL_HEIGHT * CRANK_PIPE_VISUAL_SCALE;
const CRANK_PIPE_HALF_EXTENT: f32 = CRANK_PIPE_MODEL_HALF_WIDTH * CRANK_PIPE_VISUAL_SCALE;

const CRANK_PIPE_PAIR: ArenaPipePairDefinition = ArenaPipePairDefinition {
    endpoints: [Vec2::new(-1.7, 7.0), Vec2::new(1.7, -7.0)],
    top_y: CRANK_PIPE_TOP_Y,
    collider_radius: CRANK_PIPE_HALF_EXTENT,
    trigger_radius: 0.5,
};

const CRANK_ITEMS: &[ItemAnchor] = &[
    ItemAnchor {
        kind: ItemKind::Crate,
        position: Vec3::new(-6.2, ARENA_TOP_Y + 0.5, -1.6),
        phase: 5.9,
    },
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
    PlatformDefinition::new(3.0, 4.5, 2.4, 1.45, VENT_SPIRAL_TIER_1_Y),
    PlatformDefinition::new(0.0, 5.2, 1.8, 1.45, VENT_SPIRAL_TIER_1_Y),
    PlatformDefinition::new(-3.0, 4.3, 1.8, 1.5, VENT_SPIRAL_TIER_1_Y),
    PlatformDefinition::new(-4.6, 2.0, 1.5, 2.0, VENT_SPIRAL_TIER_2_Y),
    PlatformDefinition::new(-4.4, -0.8, 1.6, 2.0, VENT_SPIRAL_TIER_2_Y),
    PlatformDefinition::new(-2.6, -3.8, 2.2, 1.45, VENT_SPIRAL_TIER_3_Y),
    PlatformDefinition::new(1.0, -4.3, 1.9, 1.45, VENT_SPIRAL_TIER_3_Y),
    PlatformDefinition::new(6.85, 2.15, 2.0, 1.15, ARENA_TOP_Y),
    PlatformDefinition::new(2.15, 6.85, 1.15, 2.0, VENT_SPIRAL_TIER_1_Y),
    PlatformDefinition::new(-6.85, -2.15, 2.0, 1.15, VENT_SPIRAL_TIER_2_Y),
    PlatformDefinition::new(-2.15, -6.85, 1.15, 2.0, VENT_SPIRAL_TIER_3_Y),
];

const VENT_SPIRAL_ITEMS: &[ItemAnchor] = &[
    ItemAnchor {
        kind: ItemKind::Steamer,
        position: Vec3::new(6.85, ARENA_TOP_Y + 0.5, 2.15),
        phase: 0.8,
    },
    ItemAnchor {
        kind: ItemKind::CupCoffee,
        position: Vec3::new(2.15, VENT_SPIRAL_TIER_1_Y + 0.5, 6.85),
        phase: 2.4,
    },
    ItemAnchor {
        kind: ItemKind::Mushroom,
        position: Vec3::new(-6.85, VENT_SPIRAL_TIER_2_Y + 0.5, -2.15),
        phase: 3.6,
    },
    ItemAnchor {
        kind: ItemKind::Turkey,
        position: Vec3::new(-2.15, VENT_SPIRAL_TIER_3_Y + 0.5, -6.85),
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

const SPLIT_HAZARDS: &[ArenaHazardDefinition] = &[
    ArenaHazardDefinition {
        kind: ArenaHazardKind::Campfire,
        center: Vec3::new(0.0, ARENA_TOP_Y + 0.07, 4.7),
        radius: 1.05,
        pulse_seconds: 1.4,
        phase: 0.0,
    },
    ArenaHazardDefinition {
        kind: ArenaHazardKind::Campfire,
        center: Vec3::new(0.0, ARENA_TOP_Y + 0.07, -4.7),
        radius: 1.05,
        pulse_seconds: 1.4,
        phase: 0.7,
    },
];

const SUNSTONE_HAZARDS: &[ArenaHazardDefinition] = &[ArenaHazardDefinition {
    kind: ArenaHazardKind::Campfire,
    center: Vec3::new(0.0, ARENA_TOP_Y + 0.25, 0.0),
    radius: 1.05,
    pulse_seconds: 1.4,
    phase: 0.0,
}];

const CRANK_HAZARDS: &[ArenaHazardDefinition] = &[
    ArenaHazardDefinition {
        kind: ArenaHazardKind::SawBlade,
        center: Vec3::new(-3.1, ARENA_TOP_Y + 0.05, 0.0),
        radius: 0.95,
        pulse_seconds: 2.1,
        phase: 0.0,
    },
    ArenaHazardDefinition {
        kind: ArenaHazardKind::SawBlade,
        center: Vec3::new(3.1, ARENA_TOP_Y + 0.05, 0.0),
        radius: 0.95,
        pulse_seconds: 2.1,
        phase: 1.05,
    },
];

const VENT_SPIRAL_HAZARDS: &[ArenaHazardDefinition] = &[
    ArenaHazardDefinition {
        kind: ArenaHazardKind::PulseVent,
        center: Vec3::new(3.4, VENT_SPIRAL_TIER_1_Y + 0.06, 4.8),
        radius: 0.82,
        pulse_seconds: 3.6,
        phase: 0.0,
    },
    ArenaHazardDefinition {
        kind: ArenaHazardKind::PulseVent,
        center: Vec3::new(-4.6, VENT_SPIRAL_TIER_2_Y + 0.06, 1.6),
        radius: 0.82,
        pulse_seconds: 3.6,
        phase: 2.4,
    },
    ArenaHazardDefinition {
        kind: ArenaHazardKind::PulseVent,
        center: Vec3::new(-1.5, VENT_SPIRAL_TIER_3_Y + 0.06, -4.1),
        radius: 0.82,
        pulse_seconds: 3.6,
        phase: 1.2,
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

const SNARE_GARDEN_HAZARDS: &[ArenaHazardDefinition] = &[];
const SKY_STEPS_HAZARDS: &[ArenaHazardDefinition] = &[];
const POWDER_KEG_HAZARDS: &[ArenaHazardDefinition] = &[];
const TRAINING_GROUND_ITEMS: &[ItemAnchor] = &[];
const TRAINING_GROUND_HAZARDS: &[ArenaHazardDefinition] = &[];
const TRAINING_GROUND_PLATFORMS: &[PlatformDefinition] = &[];

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
        pipe_pair: None,
        ringout_radius: 16.5,
        ringout_y: RINGOUT_Y,
        // Same pitch as the standard camera, moved back to frame the 24.8-wide court.
        camera_offset: Vec3::new(0.0, 18.5, 21.016),
        hazards: CROWN_HAZARDS,
        background: CROWN_RING_BACKGROUND,
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
        pipe_pair: None,
        ringout_radius: RINGOUT_RADIUS + 1.0,
        ringout_y: RINGOUT_Y,
        camera_offset: Vec3::new(0.0, 13.0, 15.2),
        hazards: SPLIT_HAZARDS,
        background: SPLIT_CAUSEWAY_BACKGROUND,
        visual_theme: ArenaVisualTheme::Causeway,
    },
    ArenaDefinition {
        name: "Sunstone Steps",
        spawn_points: [
            Vec3::new(-3.8, ARENA_TOP_Y, 2.8),
            Vec3::new(3.8, ARENA_TOP_Y, -2.8),
            Vec3::new(-3.8, ARENA_TOP_Y, -2.8),
            Vec3::new(3.8, ARENA_TOP_Y, 2.8),
        ],
        item_anchors: SUNSTONE_ITEMS,
        ground_shapes: SUNSTONE_GROUND,
        platforms: SUNSTONE_PLATFORMS,
        pipe_pair: None,
        ringout_radius: RINGOUT_RADIUS + 0.6,
        ringout_y: RINGOUT_Y - 0.25,
        camera_offset: Vec3::new(0.0, 13.6, 15.6),
        hazards: SUNSTONE_HAZARDS,
        background: SUNSTONE_STEPS_BACKGROUND,
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
        pipe_pair: Some(CRANK_PIPE_PAIR),
        ringout_radius: RINGOUT_RADIUS + 0.35,
        ringout_y: RINGOUT_Y,
        camera_offset: Vec3::new(0.0, 12.8, 14.8),
        hazards: CRANK_HAZARDS,
        background: CRANK_YARD_BACKGROUND,
        visual_theme: ArenaVisualTheme::Industrial,
    },
    ArenaDefinition {
        name: "Vent Spiral",
        spawn_points: [
            Vec3::new(-1.8, ARENA_TOP_Y, -1.8),
            Vec3::new(1.8, ARENA_TOP_Y, 1.8),
            Vec3::new(-1.8, ARENA_TOP_Y, 1.8),
            Vec3::new(1.8, ARENA_TOP_Y, -1.8),
        ],
        item_anchors: VENT_SPIRAL_ITEMS,
        ground_shapes: VENT_SPIRAL_GROUND,
        platforms: VENT_SPIRAL_PLATFORMS,
        pipe_pair: None,
        ringout_radius: RINGOUT_RADIUS + 0.45,
        ringout_y: RINGOUT_Y,
        camera_offset: Vec3::new(0.0, 13.2, 15.4),
        hazards: VENT_SPIRAL_HAZARDS,
        background: VENT_SPIRAL_BACKGROUND,
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
        pipe_pair: None,
        ringout_radius: RINGOUT_RADIUS + 0.1,
        ringout_y: RINGOUT_Y,
        camera_offset: Vec3::new(0.0, 13.4, 15.8),
        hazards: BUMPER_ALLEY_HAZARDS,
        background: BUMPER_ALLEY_BACKGROUND,
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
        pipe_pair: None,
        ringout_radius: RINGOUT_RADIUS + 0.75,
        ringout_y: RINGOUT_Y,
        camera_offset: Vec3::new(0.0, 13.0, 15.2),
        hazards: FEAST_MARKET_HAZARDS,
        background: FEAST_MARKET_BACKGROUND,
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
        pipe_pair: None,
        ringout_radius: RINGOUT_RADIUS + 0.55,
        ringout_y: RINGOUT_Y,
        camera_offset: Vec3::new(0.0, 13.3, 15.4),
        hazards: SNARE_GARDEN_HAZARDS,
        background: SNARE_GARDEN_BACKGROUND,
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
        pipe_pair: None,
        ringout_radius: RINGOUT_RADIUS + 0.85,
        ringout_y: RINGOUT_Y - 0.45,
        camera_offset: Vec3::new(0.0, 14.0, 16.2),
        hazards: SKY_STEPS_HAZARDS,
        background: SKY_STEPS_BACKGROUND,
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
        pipe_pair: None,
        ringout_radius: RINGOUT_RADIUS + 0.25,
        ringout_y: RINGOUT_Y,
        camera_offset: Vec3::new(0.0, 13.2, 15.6),
        hazards: POWDER_KEG_HAZARDS,
        background: POWDER_KEG_COURT_BACKGROUND,
        visual_theme: ArenaVisualTheme::Powder,
    },
    ArenaDefinition {
        name: "Training Ground",
        spawn_points: [
            Vec3::new(-3.8, ARENA_TOP_Y, 2.6),
            Vec3::new(3.8, ARENA_TOP_Y, 2.6),
            Vec3::new(-3.8, ARENA_TOP_Y, -2.6),
            Vec3::new(3.8, ARENA_TOP_Y, -2.6),
        ],
        item_anchors: TRAINING_GROUND_ITEMS,
        ground_shapes: TRAINING_GROUND_GROUND,
        platforms: TRAINING_GROUND_PLATFORMS,
        pipe_pair: None,
        ringout_radius: RINGOUT_RADIUS + 0.8,
        ringout_y: RINGOUT_Y,
        // Same pitch as CAMERA_BASE_OFFSET, scaled for the 18x18 court.
        camera_offset: Vec3::new(0.0, 14.1, 16.0),
        hazards: TRAINING_GROUND_HAZARDS,
        background: TRAINING_GROUND_BACKGROUND,
        visual_theme: ArenaVisualTheme::Training,
    },
];

pub(crate) fn arena_lighting_profile(index: usize) -> ArenaLightingProfile {
    let arena = arena_definition(index);
    match arena.visual_theme {
        ArenaVisualTheme::Crown => ArenaLightingProfile {
            ambient_color: Color::srgb(0.86, 0.76, 0.65),
            ambient_brightness: 410.0,
            directional_color: Color::srgb(1.0, 0.90, 0.76),
            directional_illuminance: 14_500.0,
            directional_position: Vec3::new(-8.0, 16.0, 10.0),
            point_color: Color::srgb(1.0, 0.82, 0.66),
            point_intensity: 1_200_000.0,
            point_range: 36.0,
            point_position: Vec3::new(0.0, 11.0, 5.0),
        },
        ArenaVisualTheme::Training => ArenaLightingProfile {
            ambient_color: Color::srgb(0.68, 0.64, 0.58),
            ambient_brightness: 220.0,
            directional_color: Color::srgb(1.0, 0.96, 0.90),
            directional_illuminance: 8_000.0,
            directional_position: Vec3::new(-5.0, 12.0, 7.0),
            point_color: Color::srgb(1.0, 0.86, 0.70),
            point_intensity: 1_600_000.0,
            point_range: 20.0,
            point_position: Vec3::new(0.0, 8.0, 4.0),
        },
        _ => ArenaLightingProfile {
            ambient_color: Color::srgb(0.85, 0.78, 0.68),
            ambient_brightness: 430.0,
            directional_color: Color::WHITE,
            directional_illuminance: 12_500.0,
            directional_position: Vec3::new(-5.0, 12.0, 7.0),
            point_color: Color::WHITE,
            point_intensity: 1_100_000.0,
            point_range: 36.0,
            point_position: Vec3::new(0.0, 9.0, 4.5),
        },
    }
}

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
    use crate::constants::{GRAVITY, JUMP_SPEED, LANDING_SNAP_TOLERANCE};

    #[test]
    fn arena_definitions_cover_current_stage_variety() {
        let arenas = arena_definitions();
        assert_eq!(arenas.len(), TRAINING_GROUND_ARENA_INDEX + 1);
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
        assert_eq!(arenas[TRAINING_GROUND_ARENA_INDEX].name, "Training Ground");
        assert!(arenas[2].ringout_y < RINGOUT_Y);
        assert!(arenas[3].hazards.len() >= 2);
        assert!(arenas[4].hazards.iter().any(|hazard| hazard.phase > 0.0));
        assert!(arenas[6].hazards.is_empty());
        assert!(arenas[7].hazards.is_empty());
        assert!(arenas[8].hazards.is_empty());
        assert!(arenas[9].hazards.is_empty());
        assert!(arenas[TRAINING_GROUND_ARENA_INDEX].item_anchors.is_empty());
        assert!(arenas[TRAINING_GROUND_ARENA_INDEX].hazards.is_empty());
        assert!(arenas[TRAINING_GROUND_ARENA_INDEX].platforms.is_empty());
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
    fn every_arena_has_its_own_matching_background_asset() {
        let expected_assets = [
            ("backgrounds/crown_ring.png", Vec2::new(1448.0, 1086.0)),
            ("backgrounds/split_causeway.png", Vec2::new(1536.0, 1024.0)),
            ("backgrounds/sunstone_steps.png", Vec2::new(1536.0, 1024.0)),
            ("backgrounds/crank_yard.png", Vec2::new(1536.0, 1024.0)),
            ("backgrounds/vent_spiral.png", Vec2::new(1536.0, 1024.0)),
            ("backgrounds/bumper_alley.png", Vec2::new(1536.0, 1024.0)),
            ("backgrounds/feast_market.png", Vec2::new(1536.0, 1024.0)),
            ("backgrounds/snare_garden.png", Vec2::new(1536.0, 1024.0)),
            ("backgrounds/sky_steps.png", Vec2::new(1536.0, 1024.0)),
            (
                "backgrounds/powder_keg_court.png",
                Vec2::new(1536.0, 1024.0),
            ),
            (TRAINING_GROUND_BACKGROUND_PATH, Vec2::new(1254.0, 1254.0)),
        ];

        let arenas = arena_definitions();
        assert_eq!(arenas.len(), expected_assets.len());
        for (arena, (expected_path, expected_size)) in arenas.iter().zip(expected_assets) {
            assert_eq!(arena.background.asset_path, expected_path, "{}", arena.name);
            assert_eq!(arena.background.image_size, expected_size);

            let asset_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("assets")
                .join(expected_path);
            assert!(asset_path.is_file(), "missing {}", asset_path.display());
        }

        for (index, arena) in arenas.iter().enumerate() {
            assert!(
                arenas[index + 1..]
                    .iter()
                    .all(|other| other.background.asset_path != arena.background.asset_path),
                "{} shares its background asset",
                arena.name
            );
        }
    }

    #[test]
    fn supplied_training_ground_png_keeps_its_authored_dimensions() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join(TRAINING_GROUND_BACKGROUND_PATH);
        let bytes = std::fs::read(&path).expect("training ground preview should be readable");
        assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert!(bytes.len() >= 24, "PNG is missing its IHDR header");
        assert_eq!(&bytes[12..16], b"IHDR");
        let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
        let height = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
        assert_eq!((width, height), (1254, 1254));
    }

    #[test]
    fn crown_ring_uses_the_supplied_castle_backdrop_without_stretching() {
        let crown = arena_definition(0);
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join(crown.background.asset_path);
        let bytes = std::fs::read(&path).expect("Crown Ring backdrop should be readable");
        assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert!(bytes.len() >= 24, "PNG is missing its IHDR header");
        assert_eq!(&bytes[12..16], b"IHDR");
        let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
        let height = u32::from_be_bytes(bytes[20..24].try_into().unwrap());

        assert_eq!(crown.background.asset_path, "backgrounds/crown_ring.png");
        assert_eq!((width, height), (1448, 1086));
        assert_eq!(crown.background.image_size, Vec2::new(1448.0, 1086.0));
        assert_eq!(crown.background.world_height, 84.0);
        assert!(crown.background.gameplay_visible);
    }

    #[test]
    fn training_ground_is_a_single_flat_rectangle() {
        let training = arena_definition(TRAINING_GROUND_ARENA_INDEX);
        assert_eq!(training.ground_shapes.len(), 1);
        assert!(matches!(
            training.ground_shapes[0],
            ArenaGroundShape::Rectangle {
                half_extents,
                top_y: ARENA_TOP_Y,
                ..
            } if half_extents == Vec2::new(9.0, 9.0)
        ));
        assert!(!training.background.gameplay_visible);
        assert!(
            training
                .camera_offset
                .normalize()
                .distance(CAMERA_BASE_OFFSET.normalize())
                < 0.002,
            "training ground should use the standard gameplay camera pitch"
        );
        assert!(
            arena_definitions()[..TRAINING_GROUND_ARENA_INDEX]
                .iter()
                .all(|arena| arena.background.gameplay_visible)
        );
    }

    #[test]
    fn arena_definition_clamps_selection_index() {
        assert_eq!(arena_definition(0).name, "Crown Ring");
        assert_eq!(arena_definition(1).name, "Split Causeway");
        assert_eq!(arena_definition(usize::MAX).name, "Training Ground");
    }

    #[test]
    fn split_causeway_uses_two_symmetric_campfires() {
        let split = arena_definition(1);
        assert_eq!(split.hazards.len(), 2);
        assert!(
            split
                .hazards
                .iter()
                .all(|hazard| hazard.kind == ArenaHazardKind::Campfire)
        );
        assert_eq!(split.hazards[0].center.x, 0.0);
        assert_eq!(split.hazards[1].center.x, 0.0);
        assert_eq!(split.hazards[0].center.z, -split.hazards[1].center.z);
    }

    #[test]
    fn sunstone_steps_uses_a_clear_hazard_and_symmetric_terraces() {
        let sunstone = arena_definition(2);
        assert_eq!(sunstone.hazards.len(), 1);
        assert_eq!(sunstone.hazards[0].kind, ArenaHazardKind::Campfire);
        assert_eq!(sunstone.hazards[0].center.x, 0.0);
        assert_eq!(sunstone.hazards[0].center.z, 0.0);

        for z in [-8.6, 8.6] {
            assert_eq!(
                crate::arena::ground_support_for_arena_with_radius(sunstone, 0.0, z, 0.0).height(),
                Some(ARENA_TOP_Y - 0.08)
            );
        }
    }

    #[test]
    fn crank_yard_pipe_pair_matches_standable_round_barriers() {
        let crank = arena_definition(3);
        let pipe_pair = crank.pipe_pair.expect("Crank Yard should link its pipes");

        for endpoint in pipe_pair.endpoints {
            let collider = PlatformDefinition::circle(
                endpoint.x,
                endpoint.y,
                pipe_pair.collider_radius,
                pipe_pair.top_y,
            );
            assert_eq!(collider.center, endpoint);
            assert_eq!(collider.top_y, pipe_pair.top_y);
            assert_eq!(collider.half_extents, Vec2::splat(CRANK_PIPE_HALF_EXTENT));
            assert_eq!(pipe_pair.collider_radius, CRANK_PIPE_HALF_EXTENT);
            assert!(collider.half_extents.min_element() >= pipe_pair.trigger_radius);
        }

        let expected_top = ARENA_TOP_Y + CRANK_PIPE_MODEL_HEIGHT * CRANK_PIPE_VISUAL_SCALE;
        assert_eq!(pipe_pair.top_y, expected_top);

        let weakest_jump_speed = JUMP_SPEED * 0.9;
        let weakest_jump_rise = weakest_jump_speed.powi(2) / (2.0 * GRAVITY);
        let landing_clearance = weakest_jump_rise - (pipe_pair.top_y - ARENA_TOP_Y);
        assert!(landing_clearance >= LANDING_SNAP_TOLERANCE + 0.04);
    }

    #[test]
    fn crank_yard_damage_zones_are_saw_blades_on_the_conveyors() {
        let crank = arena_definition(3);
        assert_eq!(crank.hazards.len(), 2);
        assert!(
            crank
                .hazards
                .iter()
                .all(|hazard| hazard.kind == ArenaHazardKind::SawBlade)
        );
        assert_eq!(crank.hazards[0].center.x, -3.1);
        assert_eq!(crank.hazards[1].center.x, 3.1);
        assert!(crank.hazards.iter().all(|hazard| hazard.center.z == 0.0));
    }

    #[test]
    fn vent_spiral_uses_four_distinct_jumpable_tiers() {
        let vent = arena_definition(4);
        let mut heights = vec![ARENA_TOP_Y];
        heights.extend(vent.platforms.iter().map(|platform| platform.top_y));
        heights.sort_by(f32::total_cmp);
        heights.dedup();

        assert_eq!(
            heights,
            vec![
                ARENA_TOP_Y,
                VENT_SPIRAL_TIER_1_Y,
                VENT_SPIRAL_TIER_2_Y,
                VENT_SPIRAL_TIER_3_Y,
            ]
        );
        assert!(
            heights
                .windows(2)
                .all(|pair| (pair[1] - pair[0] - VENT_SPIRAL_TIER_STEP).abs() < 0.001)
        );

        let weakest_jump_speed = JUMP_SPEED * 0.9;
        let weakest_jump_rise = weakest_jump_speed.powi(2) / (2.0 * GRAVITY);
        assert!(weakest_jump_rise - VENT_SPIRAL_TIER_STEP >= LANDING_SNAP_TOLERANCE + 0.2);
    }

    #[test]
    fn vent_spiral_turbines_and_items_match_their_surface_heights() {
        let vent = arena_definition(4);
        assert_eq!(vent.hazards.len(), 3);
        assert!(
            vent.hazards
                .iter()
                .all(|hazard| hazard.kind == ArenaHazardKind::PulseVent)
        );

        for hazard in vent.hazards {
            let support = crate::arena::ground_support_for_arena_with_radius(
                vent,
                hazard.center.x,
                hazard.center.z,
                0.0,
            )
            .height()
            .expect("vent turbine should sit on a tier");
            assert!((hazard.center.y - support - 0.06).abs() < 0.001);
        }

        for anchor in vent.item_anchors {
            let support = crate::arena::ground_support_for_arena_with_radius(
                vent,
                anchor.position.x,
                anchor.position.z,
                0.0,
            )
            .height()
            .expect("vent item should sit on a balcony");
            assert!((anchor.position.y - support - 0.5).abs() < 0.001);
            for hazard in vent.hazards {
                let item = Vec2::new(anchor.position.x, anchor.position.z);
                let turbine = Vec2::new(hazard.center.x, hazard.center.z);
                assert!(item.distance(turbine) > hazard.radius + 0.75);
            }
        }
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
    fn crown_ring_matches_the_reference_footprint_and_rear_terrace() {
        let crown = arena_definition(0);
        assert_eq!(crown.ground_shapes.len(), 10);
        assert_eq!(crown.platforms.len(), 5);

        for (x, z) in [
            (4.0, 2.0),
            (0.0, 9.4),
            (-8.25, 1.4),
            (8.25, 1.4),
            (-11.65, 1.4),
            (11.65, 1.4),
            (-9.95, -2.6),
            (9.95, 5.4),
        ] {
            assert!(
                crate::arena::ground_support_for_arena_with_radius(crown, x, z, 0.0)
                    .height()
                    .is_some(),
                "reference floor point ({x}, {z}) should be supported"
            );
        }

        for x in [-9.95, 9.95] {
            assert_eq!(
                crate::arena::ground_support_for_arena_with_radius(crown, x, 1.4, 0.0).height(),
                None,
                "the side-wing void at x={x} must remain a real opening"
            );
        }

        let expected_heights = [0.07, 0.14, 0.21, 0.28, 0.28];
        for (platform, elevation) in crown.platforms.iter().zip(expected_heights) {
            assert!((platform.top_y - (ARENA_TOP_Y + elevation)).abs() < 0.001);
        }
        assert_eq!(crown.platforms[4].center, Vec2::new(0.0, -8.85));
        assert_eq!(crown.platforms[4].half_extents, Vec2::new(5.2, 1.55));

        assert!(
            crown
                .camera_offset
                .normalize()
                .distance(CAMERA_BASE_OFFSET.normalize())
                < 0.002,
            "Crown Ring should retain the standard gameplay camera pitch"
        );
        let farthest_floor_corner = Vec2::new(12.4, 6.3).length();
        assert!(crown.ringout_radius > farthest_floor_corner);
    }

    #[test]
    fn crown_ring_uses_its_warm_reference_lighting_profile() {
        let crown = arena_lighting_profile(0);
        let default = arena_lighting_profile(1);

        assert!(crown.directional_illuminance > default.directional_illuminance);
        assert!(crown.point_intensity > default.point_intensity);
        assert_ne!(crown.directional_color, default.directional_color);
        assert_ne!(crown.point_color, default.point_color);
        assert_eq!(crown.directional_position, Vec3::new(-8.0, 16.0, 10.0));
    }
}
