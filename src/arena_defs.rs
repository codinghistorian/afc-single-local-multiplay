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
    pub platforms: &'static [PlatformDefinition],
    pub ringout_radius: f32,
    pub ringout_y: f32,
    pub camera_offset: Vec3,
    pub hazards: &'static [ArenaHazardDefinition],
    pub background: ArenaBackgroundDefinition,
}

const ANIME_SKY_BACKGROUND: ArenaBackgroundDefinition = ArenaBackgroundDefinition {
    asset_path: "backgrounds/beautiful_sky_anime.png",
    image_size: Vec2::new(1536.0, 1024.0),
    world_height: 300.0,
    position: Vec3::new(0.0, 24.0, -24.0),
};

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

const LOW_TIDE_PLATFORMS: &[PlatformDefinition] = &[
    PlatformDefinition::new(0.0, 0.0, 3.2, 2.4, ARENA_TOP_Y + 0.18),
    PlatformDefinition::new(-6.4, 3.8, 1.8, 1.2, ARENA_TOP_Y + 0.52),
    PlatformDefinition::new(6.4, -3.8, 1.8, 1.2, ARENA_TOP_Y + 0.52),
    PlatformDefinition::new(0.0, -8.6, 4.2, 1.0, ARENA_TOP_Y - 0.08),
];

const LOW_TIDE_ITEMS: &[ItemAnchor] = &[
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
        position: Vec3::new(-3.8, ARENA_TOP_Y + 0.48, -5.4),
        phase: 4.8,
    },
    ItemAnchor {
        kind: ItemKind::Steamer,
        position: Vec3::new(3.8, ARENA_TOP_Y + 0.46, 5.4),
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

const CROWN_HAZARDS: &[ArenaHazardDefinition] = &[];

const SPLIT_HAZARDS: &[ArenaHazardDefinition] = &[ArenaHazardDefinition {
    kind: ArenaHazardKind::SnareField,
    center: Vec3::new(0.0, ARENA_TOP_Y + 0.05, -4.2),
    radius: 1.65,
    pulse_seconds: 3.1,
}];

const LOW_TIDE_HAZARDS: &[ArenaHazardDefinition] = &[ArenaHazardDefinition {
    kind: ArenaHazardKind::SnareField,
    center: Vec3::new(0.0, ARENA_TOP_Y + 0.05, -5.7),
    radius: 1.8,
    pulse_seconds: 3.3,
}];

const CRANK_HAZARDS: &[ArenaHazardDefinition] = &[
    ArenaHazardDefinition {
        kind: ArenaHazardKind::BumperNode,
        center: Vec3::new(-3.1, ARENA_TOP_Y + 0.05, 0.0),
        radius: 0.95,
        pulse_seconds: 2.1,
    },
    ArenaHazardDefinition {
        kind: ArenaHazardKind::BumperNode,
        center: Vec3::new(3.1, ARENA_TOP_Y + 0.05, 0.0),
        radius: 0.95,
        pulse_seconds: 2.1,
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
        platforms: CROWN_PLATFORMS,
        ringout_radius: RINGOUT_RADIUS,
        ringout_y: RINGOUT_Y,
        camera_offset: CAMERA_BASE_OFFSET,
        hazards: CROWN_HAZARDS,
        background: ANIME_SKY_BACKGROUND,
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
        platforms: SPLIT_PLATFORMS,
        ringout_radius: RINGOUT_RADIUS + 1.0,
        ringout_y: RINGOUT_Y,
        camera_offset: Vec3::new(0.0, 13.0, 15.2),
        hazards: SPLIT_HAZARDS,
        background: ANIME_SKY_BACKGROUND,
    },
    ArenaDefinition {
        name: "Low Tide Steps",
        spawn_points: [
            Vec3::new(-4.8, ARENA_TOP_Y, 4.0),
            Vec3::new(4.8, ARENA_TOP_Y, -4.0),
            Vec3::new(-4.8, ARENA_TOP_Y, -4.0),
            Vec3::new(4.8, ARENA_TOP_Y, 4.0),
        ],
        item_anchors: LOW_TIDE_ITEMS,
        platforms: LOW_TIDE_PLATFORMS,
        ringout_radius: RINGOUT_RADIUS + 0.6,
        ringout_y: RINGOUT_Y - 0.25,
        camera_offset: Vec3::new(0.0, 13.6, 15.6),
        hazards: LOW_TIDE_HAZARDS,
        background: ANIME_SKY_BACKGROUND,
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
        platforms: CRANK_PLATFORMS,
        ringout_radius: RINGOUT_RADIUS + 0.35,
        ringout_y: RINGOUT_Y,
        camera_offset: Vec3::new(0.0, 12.8, 14.8),
        hazards: CRANK_HAZARDS,
        background: ANIME_SKY_BACKGROUND,
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
        assert!(arenas.len() >= 4);
        assert_eq!(arenas[0].spawn_points.len(), 4);
        assert!(!arenas[0].item_anchors.is_empty());
        assert!(!arenas[1].hazards.is_empty());
        assert!(arenas[1].platforms[2].top_y > ARENA_TOP_Y);
        assert_eq!(arenas[2].name, "Low Tide Steps");
        assert_eq!(arenas[3].name, "Crank Yard");
        assert!(arenas[2].ringout_y < RINGOUT_Y);
        assert!(arenas[3].hazards.len() >= 2);
    }

    #[test]
    fn arena_definition_clamps_selection_index() {
        assert_eq!(arena_definition(0).name, "Crown Ring");
        assert_eq!(arena_definition(1).name, "Split Causeway");
        assert_eq!(arena_definition(usize::MAX).name, "Crank Yard");
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
