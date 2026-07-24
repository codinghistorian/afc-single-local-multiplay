//! Canonical numeric phase boundary for the ECS-backed simulation.
//!
//! Gameplay still uses `f32` for compact, fast brawler motion. At the end of
//! every completed tick, iterative values are rounded onto the documented
//! 1/4096 grid before interpolation, hashing, snapshot capture, or the next tick.

use bevy::prelude::*;

use crate::arena::ArenaCannonBomb;
use crate::bee_skills::ActiveBeeSkill;
use crate::chick_skills::ActiveChickSkill;
#[cfg(test)]
use crate::components::DrunkStatus;
use crate::components::{
    Fighter, FighterActionState, FighterInput, FighterMotor, FighterStats, Hitbox, SimPosition,
};
use crate::determinism::{DEFAULT_F32_QUANTIZATION, canonicalize_f32};
use crate::ecs_identity::StableSimEntity;
use crate::game_state::MatchTelemetry;
use crate::items::ArenaItem;
use crate::penguin_skills::{ActivePenguinSkill, ActivePenguinSurface};
use crate::specials::ActiveSpecial;

fn scalar(value: f32) -> f32 {
    canonicalize_f32(value, DEFAULT_F32_QUANTIZATION)
}

#[cfg(test)]
fn vector2(value: Vec2) -> Vec2 {
    Vec2::new(scalar(value.x), scalar(value.y))
}

fn vector3(value: Vec3) -> Vec3 {
    Vec3::new(scalar(value.x), scalar(value.y), scalar(value.z))
}

/// Applies the canonical numeric grid after every authoritative phase has
/// completed. All mutations are field-local, so query order cannot affect it.
pub fn canonicalize_authoritative_state(
    mut telemetry: ResMut<MatchTelemetry>,
    mut fighters: Query<
        (
            &mut Fighter,
            &mut SimPosition,
            &mut FighterInput,
            &mut FighterStats,
            &mut FighterMotor,
            &mut FighterActionState,
        ),
        Without<StableSimEntity>,
    >,
    mut items: Query<&mut ArenaItem, (With<StableSimEntity>, Without<Fighter>)>,
    mut dynamic_entities: Query<
        (
            &mut SimPosition,
            Option<&mut Hitbox>,
            Option<&mut ActiveSpecial>,
            Option<&mut ActiveBeeSkill>,
            Option<&mut ActiveChickSkill>,
            Option<&mut ActivePenguinSkill>,
            Option<&mut ActivePenguinSurface>,
            Option<&mut ArenaCannonBomb>,
        ),
        (With<StableSimEntity>, Without<Fighter>, Without<ArenaItem>),
    >,
) {
    for damage in &mut telemetry.damage_by_fighter {
        *damage = scalar(*damage);
    }

    for (mut fighter, mut transform, mut input, mut stats, mut motor, mut action) in &mut fighters {
        fighter.spawn = vector3(fighter.spawn);
        transform.translation = vector3(transform.translation);

        input.movement = Vec2::new(scalar(input.movement.x), scalar(input.movement.y));

        stats.health = scalar(stats.health);
        stats.stamina = scalar(stats.stamina);
        stats.hud_flash = scalar(stats.hud_flash);
        stats.element_carry_strength = scalar(stats.element_carry_strength);

        motor.velocity = vector3(motor.velocity);
        motor.facing = vector3(motor.facing);
        motor.dash_jump_carry_speed_limit = scalar(motor.dash_jump_carry_speed_limit);
        motor.impact_speed_limit = scalar(motor.impact_speed_limit);
        motor.penguin_ice_slide_direction = motor.penguin_ice_slide_direction.map(vector3);
        motor.penguin_ice_slide_speed = scalar(motor.penguin_ice_slide_speed);
        motor.guard_counter_source = motor.guard_counter_source.map(vector3);
        motor.landing_aftermath = motor.landing_aftermath.map(|mut aftermath| {
            aftermath.horizontal_damping = scalar(aftermath.horizontal_damping);
            aftermath
        });

        action.reaction_visual_side = scalar(action.reaction_visual_side);
    }

    for mut item in &mut items {
        item.canonicalize_authoritative_floats();
    }

    for (mut transform, hitbox, special, bee, chick, penguin, surface, bomb) in
        &mut dynamic_entities
    {
        transform.translation = vector3(transform.translation);
        if let Some(mut hitbox) = hitbox {
            hitbox.power = scalar(hitbox.power);
            hitbox.str_scale = scalar(hitbox.str_scale);
            hitbox.damage = scalar(hitbox.damage);
            hitbox.knockback = scalar(hitbox.knockback);
            hitbox.vertical_knockback = scalar(hitbox.vertical_knockback);
            hitbox.base_radius = scalar(hitbox.base_radius);
            hitbox.radius = scalar(hitbox.radius);
            hitbox.spawn_origin = vector3(hitbox.spawn_origin);
            hitbox.facing = vector3(hitbox.facing);
            hitbox.base_range = scalar(hitbox.base_range);
            hitbox.range = scalar(hitbox.range);
            hitbox.vertical_offset_scale = scalar(hitbox.vertical_offset_scale);
            hitbox.ground_path_clearance = scalar(hitbox.ground_path_clearance);
            hitbox.hitstop_scale = scalar(hitbox.hitstop_scale);
            hitbox.shake_scale = scalar(hitbox.shake_scale);
        }
        if let Some(mut special) = special {
            special.facing = vector3(special.facing);
            special.velocity = vector3(special.velocity);
            special.radius = scalar(special.radius);
            special.stamina_disrupt = scalar(special.stamina_disrupt);
            special.guard_stamina_damage = scalar(special.guard_stamina_damage);
        }
        if let Some(mut skill) = bee {
            skill.facing = vector3(skill.facing);
            skill.velocity = vector3(skill.velocity);
            skill.radius = scalar(skill.radius);
            skill.guard_stamina_damage = scalar(skill.guard_stamina_damage);
            skill.size_scale = scalar(skill.size_scale);
        }
        if let Some(mut skill) = chick {
            skill.facing = vector3(skill.facing);
            skill.velocity = vector3(skill.velocity);
            skill.radius = scalar(skill.radius);
            skill.guard_stamina_damage = scalar(skill.guard_stamina_damage);
            skill.size_scale = scalar(skill.size_scale);
        }
        if let Some(mut skill) = penguin {
            skill.facing = vector3(skill.facing);
            skill.velocity = vector3(skill.velocity);
            skill.radius = scalar(skill.radius);
            skill.guard_stamina_damage = scalar(skill.guard_stamina_damage);
            skill.size_scale = scalar(skill.size_scale);
        }
        if let Some(mut surface) = surface {
            surface.facing = vector3(surface.facing);
            surface.radius = scalar(surface.radius);
            surface.size_scale = scalar(surface.size_scale);
        }
        if let Some(mut bomb) = bomb {
            bomb.velocity = vector3(bomb.velocity);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_grid_is_idempotent_and_collapses_signed_zero() {
        let once = scalar(1.234_567);
        assert_eq!(scalar(once), once);
        assert_eq!(scalar(-0.0).to_bits(), 0.0_f32.to_bits());
    }

    #[test]
    fn fighter_input_spawn_and_deferred_aftermath_join_the_tick_end_grid() {
        use crate::reactions::{QueuedAftermath, ReactionFamilyId};

        let mut app = App::new();
        app.init_resource::<MatchTelemetry>()
            .add_systems(Update, canonicalize_authoritative_state);
        let entity = app
            .world_mut()
            .spawn((
                Fighter {
                    id: 0,
                    name: "canonical fixture",
                    color: Color::WHITE,
                    spawn: Vec3::new(0.1, 0.2, 0.3),
                },
                SimPosition::new(Vec3::new(0.4, 0.5, 0.6)),
                FighterInput {
                    movement: Vec2::new(0.1, -0.2),
                    ..default()
                },
                FighterStats::default(),
                FighterMotor {
                    landing_aftermath: Some(QueuedAftermath {
                        family: ReactionFamilyId::LauncherDown,
                        getup_transition_ms: 10,
                        recover_ms: 20,
                        landing_stick_ms: 30,
                        horizontal_damping: 0.1,
                        cue: "presentation-only",
                    }),
                    ..default()
                },
                FighterActionState::default(),
                DrunkStatus::default(),
            ))
            .id();

        app.update();

        let fighter = app.world().get::<Fighter>(entity).unwrap();
        assert_eq!(fighter.spawn, vector3(Vec3::new(0.1, 0.2, 0.3)));
        let input = app.world().get::<FighterInput>(entity).unwrap();
        assert_eq!(input.movement, Vec2::new(scalar(0.1), scalar(-0.2)));
        let motor = app.world().get::<FighterMotor>(entity).unwrap();
        assert_eq!(
            motor.landing_aftermath.unwrap().horizontal_damping,
            scalar(0.1)
        );
    }

    #[test]
    fn item_authored_and_spraying_phases_join_the_tick_end_grid() {
        use crate::determinism::{FighterId, SimEntityId, SimEntityKind};
        use crate::items::{ArenaItem, ItemKind, ItemState};
        use crate::simulation::TickTimer;

        let mut item = ArenaItem::new(ItemKind::Barrel, Vec3::ZERO, 1.7);
        item.position = Vec3::new(0.1, -0.2, 0.3);
        item.state = ItemState::Spraying {
            owner: FighterId::ZERO,
            lifetime: TickTimer::from_ticks(10),
            spray_timer: TickTimer::from_ticks(2),
            spiral_phase: 0.1,
            spiral_radius: 0.3,
        };
        assert_ne!(item.snapshot_phase(), scalar(item.snapshot_phase()));
        let ItemState::Spraying {
            spiral_phase,
            spiral_radius,
            ..
        } = item.state
        else {
            unreachable!();
        };
        assert_ne!(spiral_phase, scalar(spiral_phase));
        assert_ne!(spiral_radius, scalar(spiral_radius));

        let mut app = App::new();
        app.init_resource::<MatchTelemetry>()
            .add_systems(Update, canonicalize_authoritative_state);
        let entity = app
            .world_mut()
            .spawn((
                StableSimEntity::new(SimEntityId::new(SimEntityKind::Item, 0, 1)),
                Transform::default(),
                item,
            ))
            .id();
        app.update();
        let item = app.world().get::<ArenaItem>(entity).unwrap();

        assert_eq!(item.snapshot_phase(), scalar(1.7));
        assert_eq!(item.position, vector3(Vec3::new(0.1, -0.2, 0.3)));
        let ItemState::Spraying {
            spiral_phase,
            spiral_radius,
            ..
        } = item.state
        else {
            unreachable!();
        };
        assert_eq!(spiral_phase, scalar(0.1));
        assert_eq!(spiral_radius, scalar(0.3));
    }

    #[test]
    fn bee_and_chick_snapshot_floats_all_join_the_tick_end_grid() {
        use crate::bee_skills::BeeSkillKind;
        use crate::chick_skills::ChickSkillKind;
        use crate::combat::ImpactSource;
        use crate::determinism::{FighterHitMask, FighterId, SimEntityId, SimEntityKind};
        use crate::simulation::{ElapsedTicks, TickTimer};
        use crate::styles::FighterStyleKind;
        use crate::techniques::{AttackPayloadId, AttackShapeId};

        let mut app = App::new();
        app.init_resource::<MatchTelemetry>()
            .add_systems(Update, canonicalize_authoritative_state);
        let bee = app
            .world_mut()
            .spawn((
                StableSimEntity::new(SimEntityId::new(SimEntityKind::BeeSkill, 0, 1)),
                SimPosition::new(Vec3::new(0.123_456, -0.234_567, 0.345_678)),
                ActiveBeeSkill {
                    kind: BeeSkillKind::WorkerBee,
                    owner: FighterId::ZERO,
                    owner_style: FighterStyleKind::Anchor,
                    payload_id: AttackPayloadId::BeeWorkerSting,
                    shape_id: AttackShapeId::ProjectileBolt,
                    source: ImpactSource::Projectile,
                    facing: Vec3::new(0.456_789, 0.567_891, -0.678_912),
                    velocity: Vec3::new(-1.234_567, 2.345_678, 3.456_789),
                    target: None,
                    lifetime: TickTimer::from_ticks(5),
                    age: ElapsedTicks::from_ticks(2),
                    radius: 0.123_456,
                    guard_stamina_damage: 8.123_456,
                    repeat_interval: None,
                    repeat_timer: None,
                    already_hit: FighterHitMask::default(),
                    size_scale: 1.123_456,
                },
            ))
            .id();
        let chick = app
            .world_mut()
            .spawn((
                StableSimEntity::new(SimEntityId::new(SimEntityKind::ChickSkill, 0, 1)),
                SimPosition::new(Vec3::new(-0.765_432, 0.654_321, 0.543_219)),
                ActiveChickSkill {
                    kind: ChickSkillKind::ShellChip,
                    owner: FighterId::ZERO,
                    owner_style: FighterStyleKind::Vector,
                    payload_id: Some(AttackPayloadId::ChickShellChip),
                    shape_id: AttackShapeId::ProjectileBolt,
                    source: ImpactSource::Projectile,
                    facing: Vec3::new(-0.432_198, 0.321_987, 0.219_876),
                    velocity: Vec3::new(4.567_891, -5.678_912, 6.789_123),
                    lifetime: TickTimer::from_ticks(7),
                    age: ElapsedTicks::from_ticks(3),
                    radius: 0.234_567,
                    guard_stamina_damage: 5.234_567,
                    repeat_interval: None,
                    repeat_timer: None,
                    already_hit: FighterHitMask::default(),
                    size_scale: 1.234_567,
                },
            ))
            .id();

        app.update();

        let bee_transform = app.world().get::<SimPosition>(bee).unwrap();
        let bee_skill = app.world().get::<ActiveBeeSkill>(bee).unwrap();
        assert_eq!(
            bee_transform.translation,
            vector3(Vec3::new(0.123_456, -0.234_567, 0.345_678))
        );
        assert_eq!(
            bee_skill.facing,
            vector3(Vec3::new(0.456_789, 0.567_891, -0.678_912))
        );
        assert_eq!(
            bee_skill.velocity,
            vector3(Vec3::new(-1.234_567, 2.345_678, 3.456_789))
        );
        assert_eq!(bee_skill.radius, scalar(0.123_456));
        assert_eq!(bee_skill.guard_stamina_damage, scalar(8.123_456));
        assert_eq!(bee_skill.size_scale, scalar(1.123_456));

        let chick_transform = app.world().get::<SimPosition>(chick).unwrap();
        let chick_skill = app.world().get::<ActiveChickSkill>(chick).unwrap();
        assert_eq!(
            chick_transform.translation,
            vector3(Vec3::new(-0.765_432, 0.654_321, 0.543_219))
        );
        assert_eq!(
            chick_skill.facing,
            vector3(Vec3::new(-0.432_198, 0.321_987, 0.219_876))
        );
        assert_eq!(
            chick_skill.velocity,
            vector3(Vec3::new(4.567_891, -5.678_912, 6.789_123))
        );
        assert_eq!(chick_skill.radius, scalar(0.234_567));
        assert_eq!(chick_skill.guard_stamina_damage, scalar(5.234_567));
        assert_eq!(chick_skill.size_scale, scalar(1.234_567));
    }

    #[test]
    fn vectors_use_the_same_grid_per_axis() {
        let value = vector2(Vec2::new(0.123_456, -9.876_543));
        assert_eq!(value, vector2(value));
        let scale = DEFAULT_F32_QUANTIZATION.units_per_unit() as f32;
        assert_eq!(value.x * scale, (value.x * scale).round());
    }
}
