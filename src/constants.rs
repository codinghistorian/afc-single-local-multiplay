#![allow(dead_code)]

use bevy::prelude::*;

pub const WINDOW_WIDTH: u32 = 1280;
pub const WINDOW_HEIGHT: u32 = 720;

pub const FIGHTER_COUNT: usize = 4;
pub const MATCH_SECONDS: f32 = 120.0;
pub const TIME_UP_SECONDS: f32 = 1.2;
pub const STOCK_LIVES: i32 = 3;

/// Runtime gate for the shared Pulse Dart / Trip Plate / Snap Wave / Drift Field action.
///
/// Keep the subsystem compiled while this is false so the action can be restored without
/// rebuilding its entities, payloads, effects, tuning, or saved-control compatibility.
pub const SHARED_SPECIALS_ENABLED: bool = false;

pub const ARENA_RADIUS: f32 = 8.0;
pub const ARENA_HEIGHT: f32 = 0.9;
pub const ARENA_TOP_Y: f32 = ARENA_HEIGHT * 0.5;
pub const RINGOUT_RADIUS: f32 = 14.0;
pub const RINGOUT_Y: f32 = -3.0;
pub const RINGOUT_RADIAL_WARNING_BAND: f32 = 2.4;
pub const RINGOUT_VERTICAL_WARNING_BAND: f32 = 1.8;

pub const FIGHTER_RADIUS: f32 = 0.42;
pub const FIGHTER_HEIGHT: f32 = 1.45;
pub const FIGHTER_BODY_Y: f32 = 0.72;
pub const FIGHTER_HEAD_Y: f32 = 1.5;
pub const KENNEY_CUBE_PET_SCALE: f32 = 0.84;
pub const KENNEY_CUBE_PET_GROUND_OFFSET: f32 = 0.25;

pub const MAX_HEALTH: f32 = 100.0;
pub const MAX_STAMINA: f32 = 50.0;
pub const PRACTICE_HEALTH_REFILL_DELAY: f32 = 1.0;
pub const STAMINA_REGEN_PER_SEC: f32 = 22.0;
pub const GUARD_STAMINA_PER_SEC: f32 = 18.0;
pub const DASH_STAMINA_COST: f32 = 24.0;
pub const DASH_IMPULSE: f32 = 9.85;
pub const DASH_DURATION: f32 = 0.18;
pub const DASH_HOLD_SPEED: f32 = 8.4;
pub const DASH_HOLD_ACCEL: f32 = 52.0;
pub const DASH_TRAIL_REPEAT: f32 = 0.07;
pub const DASH_SLIDE_DURATION: f32 = 0.18;
pub const DASH_SLIDE_FRICTION: f32 = 7.2;
pub const DASH_SLIDE_STOP_SPEED: f32 = 0.35;
pub const DASH_SLIDE_ACTION_DAMPING: f32 = 0.18;
pub const DASH_JUMP_CARRY_DURATION: f32 = 0.28;
pub const DASH_JUMP_MIN_FORWARD_SPEED: f32 = DASH_HOLD_SPEED;
pub const DASH_JUMP_MAX_FORWARD_SPEED: f32 = DASH_HOLD_SPEED;
pub const JUMP_SPEED: f32 = 6.7;
pub const GRAVITY: f32 = 18.5;
pub const VERTICAL_KNOCKBACK_SCALE: f32 = 1.5;
pub const LEDGE_GRACE_SECONDS: f32 = 0.12;
pub const LEDGE_SUPPORT_GRACE_SCALE: f32 = 0.32;
pub const LEDGE_SUPPORT_GRACE_MAX: f32 = 0.16;
pub const LANDING_SNAP_TOLERANCE: f32 = 0.08;

pub const GROUND_ACCEL: f32 = 33.0;
pub const AIR_ACCEL: f32 = 11.4;
pub const MAX_GROUND_SPEED: f32 = 4.18;
pub const MAX_AIR_SPEED: f32 = 4.22;
pub const GROUND_FRICTION: f32 = 14.0;
pub const AIR_FRICTION: f32 = 1.5;

pub const LIGHT_STARTUP: f32 = 0.12;
pub const LIGHT_ACTIVE: f32 = 0.12;
pub const LIGHT_RECOVERY: f32 = 0.24;
pub const LIGHT_DAMAGE: f32 = 9.0;
pub const LIGHT_KNOCKBACK: f32 = 4.6;
pub const LIGHT_RANGE: f32 = 0.82;
pub const LIGHT_RADIUS: f32 = 0.62;

pub const LIGHT2_STARTUP: f32 = 0.14;
pub const LIGHT2_ACTIVE: f32 = 0.12;
pub const LIGHT2_RECOVERY: f32 = 0.25;
pub const LIGHT2_DAMAGE: f32 = 10.0;
pub const LIGHT2_KNOCKBACK: f32 = 5.1;
pub const LIGHT2_RANGE: f32 = 0.88;
pub const LIGHT2_RADIUS: f32 = 0.64;

pub const COMBO_FINISHER_STARTUP: f32 = 0.2;
pub const COMBO_FINISHER_ACTIVE: f32 = 0.14;
pub const COMBO_FINISHER_RECOVERY: f32 = 0.34;
pub const COMBO_FINISHER_DAMAGE: f32 = 15.0;
pub const COMBO_FINISHER_KNOCKBACK: f32 = 7.2;
pub const COMBO_FINISHER_RANGE: f32 = 1.0;
pub const COMBO_FINISHER_RADIUS: f32 = 0.72;

pub const COMBO_QUEUE_START: f32 = 0.14;
pub const COMBO_QUEUE_END: f32 = 0.39;

pub const HEAVY_STARTUP: f32 = 0.28;
pub const HEAVY_ACTIVE: f32 = 0.16;
pub const HEAVY_RECOVERY: f32 = 0.42;
pub const HEAVY_DAMAGE: f32 = 20.0;
pub const HEAVY_KNOCKBACK: f32 = 8.2;
pub const HEAVY_RANGE: f32 = 1.05;
pub const HEAVY_RADIUS: f32 = 0.76;

pub const ULTIMATE_STAMINA_COST: f32 = MAX_STAMINA * 0.5;
pub const CHICK_X_STAMINA_COST: f32 = MAX_STAMINA * 0.15;
pub const ULTIMATE_LOCK_DISTANCE: f32 = 0.74;
pub const ULTIMATE_LOCK_RELEASE_AFTER: f32 = 0.9;
pub const ULTIMATE_CATCH_DAMAGE: f32 = 4.0;
pub const ULTIMATE_SCRATCH_LIGHT_DAMAGE: f32 = 5.0;
pub const ULTIMATE_SCRATCH_HEAVY_DAMAGE: f32 = 6.0;
pub const ULTIMATE_BOMB_DAMAGE: f32 = 14.0;
pub const ULTIMATE_CATCH_RANGE: f32 = 0.86;
pub const ULTIMATE_CATCH_RADIUS: f32 = 0.54;
pub const ULTIMATE_SCRATCH_RANGE: f32 = 0.78;
pub const ULTIMATE_SCRATCH_RADIUS: f32 = 0.48;
pub const ULTIMATE_BOMB_RADIUS: f32 = 0.86;

pub const GRAB_STARTUP: f32 = 0.18;
pub const GRAB_ACTIVE: f32 = 0.12;
pub const GRAB_RECOVERY: f32 = 0.34;
pub const GRAB_DAMAGE: f32 = 5.0;
pub const GRAB_KNOCKBACK: f32 = 7.8;
pub const GRAB_RANGE: f32 = 0.62;
pub const GRAB_RADIUS: f32 = 0.48;
pub const GRAB_HOLD_MAX: f32 = 0.64;
pub const GRAB_ESCAPE_AFTER: f32 = 0.2;
pub const GRAB_REGRAB_LOCKOUT: f32 = 0.85;
pub const THROW_QUICK_DAMAGE: f32 = 5.0;
pub const THROW_STANDARD_DAMAGE: f32 = 8.0;
pub const THROW_HEAVY_DAMAGE: f32 = 11.0;
pub const THROW_QUICK_KNOCKBACK: f32 = 6.8;
pub const THROW_STANDARD_KNOCKBACK: f32 = 8.4;
pub const THROW_HEAVY_KNOCKBACK: f32 = 10.0;
pub const THROW_BRACE_SCALE: f32 = 0.72;
pub const THROW_EDGE_PRESSURE_START: f32 = 5.8;
pub const THROW_EDGE_PRESSURE_BONUS: f32 = 1.22;

pub const SPECIAL_CAST_DURATION: f32 = 0.28;
pub const SPECIAL_COOLDOWN: f32 = 1.25;
pub const SPECIAL_OWNER_GRACE: f32 = 0.18;
pub const SPECIAL_PROJECTILE_COST: f32 = 20.0;
pub const SPECIAL_PROJECTILE_SPEED: f32 = 10.8;
pub const SPECIAL_PROJECTILE_LIFETIME: f32 = 1.5;
pub const SPECIAL_PROJECTILE_RADIUS: f32 = 0.38;
pub const SPECIAL_PROJECTILE_DAMAGE: f32 = 8.0;
pub const SPECIAL_PROJECTILE_KNOCKBACK: f32 = 5.8;
pub const SPECIAL_TRAP_COST: f32 = 24.0;
pub const SPECIAL_TRAP_LIFETIME: f32 = 5.5;
pub const SPECIAL_TRAP_RADIUS: f32 = 0.78;
pub const SPECIAL_TRAP_DAMAGE: f32 = 7.0;
pub const SPECIAL_TRAP_KNOCKBACK: f32 = 4.6;
pub const SPECIAL_SHOCKWAVE_COST: f32 = 32.0;
pub const SPECIAL_SHOCKWAVE_LIFETIME: f32 = 0.32;
pub const SPECIAL_SHOCKWAVE_RADIUS: f32 = 2.65;
pub const SPECIAL_SHOCKWAVE_DAMAGE: f32 = 7.0;
pub const SPECIAL_SHOCKWAVE_KNOCKBACK: f32 = 6.2;
pub const SPECIAL_HAZARD_COST: f32 = 30.0;
pub const SPECIAL_HAZARD_LIFETIME: f32 = 3.8;
pub const SPECIAL_HAZARD_RADIUS: f32 = 1.05;
pub const SPECIAL_HAZARD_TICK: f32 = 0.48;
pub const SPECIAL_HAZARD_DAMAGE: f32 = 5.0;
pub const SPECIAL_HAZARD_KNOCKBACK: f32 = 3.8;

pub const DASH_ATTACK_STARTUP: f32 = 0.07;
pub const DASH_ATTACK_ACTIVE: f32 = 0.14;
pub const DASH_ATTACK_RECOVERY: f32 = 0.27;
pub const DASH_ATTACK_DAMAGE: f32 = 12.0;
pub const DASH_ATTACK_KNOCKBACK: f32 = 6.9;
pub const DASH_ATTACK_RANGE: f32 = 1.02;
pub const DASH_ATTACK_RADIUS: f32 = 0.68;
pub const DASH_ATTACK_EXTRA_IMPULSE: f32 = 3.0;

pub const JUMP_ATTACK_STARTUP: f32 = 0.1;
pub const JUMP_ATTACK_ACTIVE: f32 = 0.18;
pub const JUMP_ATTACK_RECOVERY: f32 = 0.18;
pub const JUMP_ATTACK_DAMAGE: f32 = 11.0;
pub const JUMP_ATTACK_KNOCKBACK: f32 = 5.8;
pub const JUMP_ATTACK_RANGE: f32 = 0.78;
pub const JUMP_ATTACK_RADIUS: f32 = 0.64;
pub const JUMP_ATTACK_VERTICAL_STALL: f32 = 1.05;
pub const JUMP_ATTACK_DIVE_FORWARD_SPEED: f32 = 5.6;
pub const JUMP_ATTACK_DIVE_DOWN_SPEED: f32 = 8.8;
pub const BEE_JUMP_ATTACK_FORWARD_SPEED: f32 = 12.0;
pub const BEE_JUMP_ATTACK_UP_SPEED: f32 = 3.8;
pub const JUMP_ATTACK_LANDING_HITBOX_LINGER: f32 = 0.12;
pub const JUMP_ATTACK_MAX_ACTIVE: f32 = 1.6;
pub const JUMP_ATTACK_LANDING_RECOVERY: f32 = 0.2;
pub const JUMP_HEAVY_AIR_STALL_PLANAR_SCALE: f32 = 0.28;
pub const JUMP_HEAVY_AIR_STALL_UP_SPEED: f32 = 0.65;
pub const JUMP_HEAVY_AIR_STALL_DOWN_SPEED: f32 = -0.55;
pub const JUMP_HEAVY_DAMAGE: f32 = 13.0;
pub const JUMP_HEAVY_KNOCKBACK: f32 = 12.4;
pub const JUMP_HEAVY_FISH_RADIUS: f32 = 0.5;
pub const JUMP_HEAVY_FISH_GROUND_CLEARANCE: f32 = 0.08;

pub const GUARD_COUNTER_STARTUP: f32 = 0.07;
pub const GUARD_COUNTER_ACTIVE: f32 = 0.12;
pub const GUARD_COUNTER_RECOVERY: f32 = 0.24;
pub const GUARD_COUNTER_DAMAGE: f32 = 10.0;
pub const GUARD_COUNTER_KNOCKBACK: f32 = 6.4;
pub const GUARD_COUNTER_RANGE: f32 = 0.9;
pub const GUARD_COUNTER_RADIUS: f32 = 0.62;
pub const GUARD_COUNTER_WINDOW: f32 = 0.6;
pub const GUARD_COUNTER_HEALTH_COST: f32 = 4.0;
pub const GUARD_COUNTER_FLASH_DURATION: f32 = 0.14;

pub const GUARD_MAX_DURATION: f32 = 1.5;
pub const GUARD_CHORD_GRACE: f32 = 0.08;
pub const GUARD_START_BUFFER_SECONDS: f32 = 0.12;
pub const GUARD_RESTART_COOLDOWN: f32 = 0.1;
pub const GUARD_HEALTH_DAMAGE_SCALE: f32 = 0.01;
pub const HITSTUN_LIGHT: f32 = 0.33;
pub const HITSTUN_HEAVY: f32 = 0.52;
pub const KNOCKDOWN_DURATION: f32 = 0.72;
pub const GETUP_DURATION: f32 = 0.34;
pub const GETUP_INVULNERABLE: f32 = 0.45;
pub const GUARD_BREAK_DURATION: f32 = 0.74;
pub const PERFECT_GUARD_WINDOW: f32 = 0.12;
pub const PERFECT_GUARD_STAMINA_RESTORE: f32 = 16.0;
pub const GUARD_STEP_STAMINA_COST: f32 = 18.0;
pub const GUARD_STEP_DURATION: f32 = 0.22;
pub const GUARD_STEP_IMPULSE: f32 = 6.2;
pub const GUARD_STEP_INVULNERABLE: f32 = 0.16;
pub const QUICK_STAND_AFTER: f32 = 0.18;
pub const QUICK_STAND_DURATION: f32 = 0.18;
pub const RECOVERY_ROLL_DURATION: f32 = 0.34;
pub const RECOVERY_ROLL_IMPULSE: f32 = 5.9;
pub const RECOVERY_ROLL_INVULNERABLE: f32 = 0.28;
pub const RESPAWN_DELAY: f32 = 1.5;
pub const RESPAWN_INVULNERABLE: f32 = 1.6;

pub const ITEM_PICKUP_RANGE: f32 = 0.86;
pub const ITEM_PICKUP_CONE_DOT: f32 = -0.15;
pub const ITEM_PICKUP_DURATION: f32 = 0.18;
pub const ITEM_DROP_DURATION: f32 = 0.18;
pub const ITEM_THROW_DURATION: f32 = 0.3;
pub const ITEM_SWING_STARTUP: f32 = 0.16;
pub const ITEM_SWING_ACTIVE: f32 = 0.16;
pub const ITEM_SWING_RECOVERY: f32 = 0.28;
pub const ITEM_RESPAWN_SECONDS: f32 = 10.0;
pub const ITEM_THROW_LIFETIME: f32 = 2.2;
pub const ITEM_MALLET_DURABILITY: i32 = 4;
pub const ITEM_MALLET_THROW_SPEED: f32 = 10.6;
pub const ITEM_MALLET_THROW_ARC: f32 = 1.1;
pub const ITEM_MALLET_THROW_GRACE: f32 = 0.16;
pub const ITEM_MALLET_PICKUP_LOCKOUT: f32 = 0.34;
pub const ITEM_BOMB_THROW_SPEED: f32 = 9.0;
pub const ITEM_BOMB_THROW_ARC: f32 = 1.25;
pub const ITEM_BOMB_THROW_GRACE: f32 = 0.2;
pub const ITEM_BOMB_PICKUP_LOCKOUT: f32 = 0.56;
pub const ITEM_GUARD_BATTERY_PICKUP_LOCKOUT: f32 = 0.28;
pub const ITEM_SPARK_LOBBER_DURABILITY: i32 = 3;
pub const ITEM_SPARK_LOBBER_THROW_SPEED: f32 = 10.2;
pub const ITEM_SPARK_LOBBER_THROW_ARC: f32 = 0.85;
pub const ITEM_SPARK_LOBBER_PICKUP_LOCKOUT: f32 = 0.38;
pub const ITEM_BREEZE_BUOY_DURABILITY: i32 = 1;
pub const ITEM_BREEZE_BUOY_STAMINA: f32 = 38.0;
pub const ITEM_BREEZE_BUOY_PICKUP_LOCKOUT: f32 = 0.42;
pub const ITEM_STONE_CRATE_DURABILITY: i32 = 2;
pub const ITEM_STONE_CRATE_THROW_SPEED: f32 = 8.0;
pub const ITEM_STONE_CRATE_THROW_ARC: f32 = 1.0;
pub const ITEM_STONE_CRATE_PICKUP_LOCKOUT: f32 = 0.62;
pub const ITEM_GUARD_KITE_DURABILITY: i32 = 5;
pub const ITEM_GUARD_KITE_THROW_SPEED: f32 = 8.8;
pub const ITEM_GUARD_KITE_THROW_ARC: f32 = 0.9;
pub const ITEM_GUARD_KITE_PICKUP_LOCKOUT: f32 = 0.32;
pub const ITEM_APPLE_HEALTH: f32 = 25.0;
pub const ITEM_WINE_WHITE_STAMINA: f32 = ULTIMATE_STAMINA_COST;
pub const ITEM_TURKEY_HEALTH: f32 = 20.0;
pub const ITEM_BARREL_STAMINA: f32 = 18.0;
pub const BARREL_SPRAY_DURATION: f32 = 4.0;
pub const BARREL_SPRAY_CADENCE: f32 = 0.25;
pub const BARREL_SPRAY_RADIUS: f32 = 2.5;
pub const DRUNK_DURATION: f32 = 5.0;
pub const DRUNK_BUBBLE_CADENCE: f32 = 0.34;
pub const ITEM_COFFEE_SPEED_SECONDS: f32 = 10.0;
pub const ITEM_COFFEE_SPEED_MULTIPLIER: f32 = 1.2;
pub const ITEM_MUSHROOM_GIANT_SECONDS: f32 = 15.0;
pub const ITEM_GIANT_SIZE_MULTIPLIER: f32 = 1.5;
pub const ITEM_GIANT_DAMAGE_TAKEN_MULTIPLIER: f32 = 0.5;
pub const ITEM_DROP_ROLL_LIFETIME: f32 = 0.9;
pub const ITEM_DROP_ROLL_PICKUP_LOCKOUT: f32 = 0.45;
pub const ITEM_MALLET_DAMAGE: f32 = 14.0;
pub const ITEM_MALLET_KNOCKBACK: f32 = 7.1;
pub const ITEM_MALLET_RANGE: f32 = 1.0;
pub const ITEM_MALLET_RADIUS: f32 = 0.72;
pub const ITEM_THROW_DAMAGE: f32 = 11.0;
pub const ITEM_THROW_KNOCKBACK: f32 = 8.0;
pub const ITEM_THROW_RADIUS: f32 = 0.48;
pub const POP_BOMB_FUSE: f32 = 0.7;
pub const POP_BOMB_BLAST_MESH_RADIUS: f32 = 0.42;
pub const POP_BOMB_BLAST_VISUAL_END_SCALE: f32 = 3.2;
pub const POP_BOMB_RADIUS: f32 = POP_BOMB_BLAST_MESH_RADIUS * POP_BOMB_BLAST_VISUAL_END_SCALE;
pub const POP_BOMB_DAMAGE: f32 = 12.0;
pub const POP_BOMB_KNOCKBACK: f32 = 7.2;

#[cfg(test)]
#[test]
fn pop_bomb_radius_matches_blast_visual_radius() {
    assert_eq!(
        POP_BOMB_RADIUS,
        POP_BOMB_BLAST_MESH_RADIUS * POP_BOMB_BLAST_VISUAL_END_SCALE
    );
}

pub const CAMERA_BASE_OFFSET: Vec3 = Vec3::new(0.0, 12.5, 14.2);
pub const CAMERA_FOLLOW_RATE: f32 = 4.4;

pub const FIGHTER_NAMES: [&str; 4] = ["Rookie Red", "Bolt Blue", "Mint Bot", "Pink Bot"];

pub const FIGHTER_COLORS: [Color; 4] = [
    Color::srgb(0.95, 0.12, 0.11),
    Color::srgb(0.12, 0.42, 1.0),
    Color::srgb(0.15, 0.9, 0.62),
    Color::srgb(1.0, 0.32, 0.72),
];
